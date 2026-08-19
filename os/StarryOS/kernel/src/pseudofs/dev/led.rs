use alloc::{borrow::Cow, boxed::Box, format, sync::Arc};

use ax_lazyinit::LazyLock;
use axfs_ng_vfs::{NodePermission, VfsError, VfsResult};
use rdif_pinctrl::{
    ConfigSetting, FunctionId, GroupId, Interface, MuxSetting, MuxValue, PinConfig, PinId,
    PinState, PinctrlDevice, StateName,
};

use crate::{
    pseudofs::{
        DirMaker, DirectRwFsFileOps, NodeOpsMux, RwFile, SimpleDir, SimpleDirOps, SimpleFile,
        SimpleFileOperation, SimpleFileOps, SimpleFs, SpecialFsFile,
    },
    sync::Mutex,
};

const LEDS: [LedDesc; 2] = [
    LedDesc {
        name: "blue_led",
        pin: 102, // GPIO3_A6
    },
    LedDesc {
        name: "green_led",
        pin: 105, // GPIO3_B1
    },
];

#[derive(Clone, Copy)]
struct LedDesc {
    name: &'static str,
    pin: u32,
}

struct LedState {
    brightness: [bool; LEDS.len()],
}

static LED_STATE: LazyLock<Mutex<LedState>> = LazyLock::new(|| {
    Mutex::new(LedState {
        brightness: [false; LEDS.len()],
    })
});

pub(crate) fn led_class_dir_maker(fs: Arc<SimpleFs>) -> DirMaker {
    SimpleDir::new_maker(fs.clone(), Arc::new(LedClassDir { fs }))
}

struct LedClassDir {
    fs: Arc<SimpleFs>,
}

impl SimpleDirOps for LedClassDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(LEDS.iter().map(|led| Cow::Borrowed(led.name)))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let index = LEDS
            .iter()
            .position(|led| led.name == name)
            .ok_or(VfsError::NotFound)?;
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            self.fs.clone(),
            Arc::new(LedDir {
                fs: self.fs.clone(),
                index,
            }),
        )))
    }
}

struct LedDir {
    fs: Arc<SimpleFs>,
    index: usize,
}

impl SimpleDirOps for LedDir {
    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        Box::new(
            ["brightness", "max_brightness"]
                .into_iter()
                .map(Cow::Borrowed),
        )
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        match name {
            "brightness" => Ok(led_attr_file(
                self.fs.clone(),
                RwFile::new({
                    let index = self.index;
                    move |req| match req {
                        SimpleFileOperation::Read => {
                            let value = LED_STATE.lock().brightness[index] as u8;
                            Ok(Some(format!("{value}\n").into_bytes()))
                        }
                        SimpleFileOperation::Write(data) => {
                            let on = parse_brightness(data)?;
                            set_led(index, on)?;
                            Ok(None)
                        }
                    }
                }),
            )
            .into()),
            "max_brightness" => Ok(SimpleFile::new_regular(self.fs.clone(), || Ok("1\n")).into()),
            _ => Err(VfsError::NotFound),
        }
    }
}

struct LedAttrFile {
    ops: Arc<dyn SimpleFileOps>,
}

impl DirectRwFsFileOps for LedAttrFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let data = self.ops.read_all()?;
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let data = &data[offset as usize..];
        let read = data.len().min(buf.len());
        buf[..read].copy_from_slice(&data[..read]);
        Ok(read)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        if offset != 0 {
            return Err(VfsError::InvalidInput);
        }
        self.ops.write_all(buf)?;
        Ok(buf.len())
    }
}

fn led_attr_file(fs: Arc<SimpleFs>, ops: impl SimpleFileOps) -> Arc<SpecialFsFile<LedAttrFile>> {
    SpecialFsFile::new_regular_with_perm(
        fs,
        LedAttrFile { ops: Arc::new(ops) },
        NodePermission::default(),
    )
}

fn parse_brightness(data: &[u8]) -> VfsResult<bool> {
    match core::str::from_utf8(data).map(str::trim) {
        Ok("0") => Ok(false),
        Ok("1") => Ok(true),
        _ => Err(VfsError::InvalidInput),
    }
}

fn set_led(index: usize, on: bool) -> VfsResult<()> {
    let led = LEDS.get(index).ok_or(VfsError::InvalidInput)?;
    let pin = PinId::new(led.pin);
    let state = PinState::named(StateName::Default)
        .with_mux(MuxSetting::new(
            GroupId::new(led.pin),
            FunctionId::new(0),
            MuxValue::new(0),
        ))
        .with_config(ConfigSetting::pin(pin, PinConfig::OutputValue(on)))
        .with_config(ConfigSetting::pin(pin, PinConfig::OutputEnable(true)));

    let device = rdrive::get_one::<PinctrlDevice>().ok_or_else(|| {
        warn!("cannot set {}: PinctrlDevice is unavailable", led.name);
        VfsError::Io
    })?;
    let mut pinctrl = device.lock().map_err(|err| {
        warn!("cannot lock PinctrlDevice for {}: {err}", led.name);
        VfsError::Io
    })?;
    pinctrl.apply_state(&state).map_err(|err| {
        warn!("cannot set {} to {on}: {err}", led.name);
        VfsError::Io
    })?;
    LED_STATE.lock().brightness[index] = on;
    Ok(())
}

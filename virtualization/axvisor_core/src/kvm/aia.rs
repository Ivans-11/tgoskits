//! Software AIA control device: flat IMSIC topology and up to 63 wired sources.
//! State is shared with CSR emulation; all host kicks happen outside Core locks.
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use axvisor_api::control as api;
use riscv_aia::Aplic;
use riscv_vcpu::aia::{Context, ImsicFile};

use super::{
    CONTROL_FILES, ControlFileState, KVM_CONTROL_OPS, next_control_file_id,
    util::{read_u32_user, read_u64_user, write_u32_user, write_u64_user},
};

pub(super) struct Aia {
    mode: u32,
    ids: usize,
    sources: usize,
    aplic_addr: Option<u64>,
    imsic_addrs: BTreeMap<usize, u64>,
    files: BTreeMap<usize, Arc<ImsicFile>>,
    aplic: Aplic,
    initialized: bool,
}

pub(super) fn available() -> bool {
    riscv_vcpu::detect_aia()
}

fn state(vm_file: api::ControlFileId) -> AxResult<Arc<SpinNoIrq<Aia>>> {
    match CONTROL_FILES.lock().get(&vm_file) {
        Some(ControlFileState::Vm(vm)) => vm.aia.clone().ok_or(AxError::BadState),
        _ => Err(AxError::NotFound),
    }
}

pub(super) fn create_device(vm_file: api::ControlFileId, arg: usize) -> AxResult<isize> {
    if read_u32_user(arg)? != 11 || !available() {
        return Err(AxError::Unsupported);
    }
    match read_u32_user(arg + 8)? {
        1 => return Ok(0),
        0 => {}
        _ => return Err(AxError::InvalidInput),
    }
    let id = next_control_file_id()?;
    {
        let mut files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vm(vm)) = files.get_mut(&vm_file) else {
            return Err(AxError::NotFound);
        };
        if vm.aia.is_some() {
            return Err(AxError::AlreadyExists);
        }
        if vm.vm.running() {
            return Err(AxError::BadState);
        }
        vm.aia = Some(Arc::new(SpinNoIrq::new(Aia {
            mode: 0,
            ids: 63,
            sources: 0,
            aplic_addr: None,
            imsic_addrs: BTreeMap::new(),
            files: BTreeMap::new(),
            aplic: Aplic::new(0).unwrap(),
            initialized: false,
        })));
        vm.aia_device = Some(id);
        files.insert(id, ControlFileState::AiaDevice(vm_file));
    }
    let fd = match api::create_user_fd(id, KVM_CONTROL_OPS, None) {
        Ok(fd) => fd,
        Err(err) => {
            let mut files = CONTROL_FILES.lock();
            files.remove(&id);
            if let Some(ControlFileState::Vm(vm)) = files.get_mut(&vm_file) {
                vm.aia = None;
                vm.aia_device = None;
            }
            return Err(err);
        }
    };
    write_u32_user(arg + 4, fd as u32)?;
    Ok(0)
}

pub(super) fn device_ioctl(vm_file: api::ControlFileId, cmd: u32, arg: usize) -> AxResult<isize> {
    let flags = read_u32_user(arg)?;
    let group = read_u32_user(arg + 4)?;
    let attr = read_u64_user(arg + 8)?;
    let addr = read_u64_user(arg + 16)? as usize;
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }
    if !matches!(
        (group, attr),
        (0, 0..=6) | (1, 0..=64) | (2, 0) | (3, 0..=0x3ffc) | (4, _)
    ) {
        return Err(AxError::Unsupported);
    }
    let shared = state(vm_file)?;
    if cmd == super::KVM_HAS_DEVICE_ATTR {
        if group == 3 && attr & 3 != 0 {
            return Err(AxError::InvalidInput);
        }
        if group == 4 {
            let aia = shared.lock();
            let probe = riscv_aia::Imsic::new(aia.ids).unwrap();
            if attr >> 12 >= 64 || probe.read((attr & 0xfff) as usize).is_none() {
                return Err(AxError::InvalidInput);
            }
        }
        return Ok(0);
    }
    if cmd != super::KVM_SET_DEVICE_ATTR && cmd != super::KVM_GET_DEVICE_ATTR {
        return Err(AxError::Unsupported);
    }
    let write = cmd == super::KVM_SET_DEVICE_ATTR;
    if group == 2 && write {
        return initialize(vm_file);
    }
    let value = if write && group != 2 {
        if matches!(group, 1 | 4) {
            read_u64_user(addr)?
        } else {
            u64::from(read_u32_user(addr)?)
        }
    } else {
        0
    };
    let mut aia = shared.lock();
    let mut pending = Vec::new();
    let result = match group {
        0 => {
            let old = match attr {
                0 => u64::from(aia.mode),
                1 => aia.ids as u64,
                2 => aia.sources as u64,
                4 => 24,
                5 => 6,
                _ => 0,
            };
            if write {
                if aia.initialized {
                    return Err(AxError::BadState);
                }
                match (attr, value) {
                    (0, 0 | 2) => aia.mode = value as u32,
                    (3 | 6, 0) | (4, 24) | (5, 6) => {}
                    (1, 63..=2047) if value & 63 == 63 => aia.ids = value as usize,
                    (2, 0..=63) => aia.sources = value as usize,
                    _ => return Err(AxError::InvalidInput),
                }
            }
            old
        }
        1 => {
            if write {
                if aia.initialized {
                    return Err(AxError::BadState);
                }
                let alignment = if attr == 0 { 0x4000 } else { 0x1000 };
                if value & (alignment - 1) != 0 || value.checked_add(alignment).is_none() {
                    return Err(AxError::InvalidInput);
                }
                if attr == 0 {
                    aia.aplic_addr = Some(value);
                } else {
                    aia.imsic_addrs.insert(attr as usize - 1, value);
                }
                0
            } else if attr == 0 {
                aia.aplic_addr.ok_or(AxError::BadState)?
            } else {
                *aia.imsic_addrs
                    .get(&(attr as usize - 1))
                    .ok_or(AxError::InvalidInput)?
            }
        }
        3 if aia.initialized => {
            if attr & 3 != 0 {
                return Err(AxError::InvalidInput);
            }
            if write {
                aia.aplic.write(attr as usize, value as u32);
                pending = drain(&mut aia);
            }
            u64::from(aia.aplic.read(attr as usize))
        }
        4 if aia.initialized => {
            let file = aia
                .files
                .get(&((attr >> 12) as usize))
                .ok_or(AxError::InvalidInput)?
                .clone();
            drop(aia);
            let selector = (attr & 0xfff) as usize;
            if write {
                if !file.update(|imsic| imsic.write(selector, value as usize))? {
                    return Err(AxError::InvalidInput);
                }
            } else {
                let value = file
                    .update(|imsic| imsic.read(selector))?
                    .ok_or(AxError::InvalidInput)?;
                write_u64_user(addr, value as u64)?;
            }
            return Ok(0);
        }
        _ => return Err(AxError::Unsupported),
    };
    drop(aia);
    dispatch(pending)?;
    if !write {
        if group == 1 {
            write_u64_user(addr, result)?;
        } else {
            write_u32_user(addr, result as u32)?;
        }
    }
    Ok(0)
}

fn initialize(vm_file: api::ControlFileId) -> AxResult<isize> {
    let stateen = riscv_vcpu::detect_stateen();
    let mut controls = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = controls.get(&vm_file) else {
        return Err(AxError::NotFound);
    };
    let shared = vm.aia.clone().ok_or(AxError::BadState)?;
    let mut aia = shared.lock();
    if aia.initialized
        || vm.vm.running()
        || vm.vcpu_files.is_empty()
        || (aia.sources != 0 && aia.aplic_addr.is_none())
    {
        return Err(AxError::BadState);
    }
    let mut guards = Vec::new();
    for id in vm.vcpu_files.values() {
        let Some(ControlFileState::Vcpu(vcpu)) = controls.get(id) else {
            return Err(AxError::NotFound);
        };
        guards.push(
            vcpu.operation_lock
                .try_lock()
                .ok_or(AxError::ResourceBusy)?,
        );
    }
    for (id, address) in &aia.imsic_addrs {
        if !vm.vcpu_files.contains_key(&(*id as u32)) {
            return Err(AxError::InvalidInput);
        }
        if let Some(base) = aia.aplic_addr
            && *address < base + 0x4000
            && address + 0x1000 > base
        {
            return Err(AxError::InvalidInput);
        }
        if aia
            .imsic_addrs
            .iter()
            .any(|(other, addr)| other != id && ((addr >> 12) & 63) == ((address >> 12) & 63))
        {
            return Err(AxError::InvalidInput);
        }
    }
    if aia.imsic_addrs.len() != vm.vcpu_files.len() {
        return Err(AxError::InvalidInput);
    }
    for id in vm.vcpu_files.keys() {
        let file = ImsicFile::new(aia.ids, aia.mode == 2).ok_or(AxError::InvalidInput)?;
        let vcpu = vm.vm.vcpu(*id as usize).ok_or(AxError::NotFound)?;
        vcpu.get_arch_vcpu().aia = Some(Context::new(file.clone(), stateen));
        aia.files.insert(*id as usize, file);
    }
    aia.aplic = Aplic::new(aia.sources).unwrap();
    aia.initialized = true;
    drop(aia);
    if let Some(ControlFileState::Vm(vm)) = controls.get_mut(&vm_file) {
        vm.irqchip_created = true;
    }
    drop(controls);
    drop(guards);
    Ok(0)
}

type Pending = Vec<(Arc<ImsicFile>, usize)>;
fn drain(aia: &mut Aia) -> Pending {
    let mut pending = Vec::new();
    let files = &aia.files;
    let addresses = &aia.imsic_addrs;
    aia.aplic.drain(|hart, id| {
        if let Some((vcpu, _)) = addresses
            .iter()
            .find(|(_, addr)| ((*addr >> 12) & 63) as usize == hart)
            && let Some(file) = files.get(vcpu)
        {
            pending.push((file.clone(), id));
        }
    });
    pending
}
fn dispatch(pending: Pending) -> AxResult {
    for (file, id) in pending {
        file.inject(id)?;
    }
    Ok(())
}

pub(super) fn pulse(vm_file: api::ControlFileId, source: u32) -> AxResult {
    let shared = state(vm_file)?;
    let pending = {
        let mut aia = shared.lock();
        if !aia.initialized || !aia.aplic.set_level(source as usize, true) {
            return Err(AxError::InvalidInput);
        }
        let mut pending = drain(&mut aia);
        aia.aplic.set_level(source as usize, false);
        pending.extend(drain(&mut aia));
        pending
    };
    dispatch(pending)
}

pub(super) fn inject_msi(vm_file: api::ControlFileId, address: u64, data: u32) -> AxResult {
    let shared = state(vm_file)?;
    let file = {
        let aia = shared.lock();
        if !aia.initialized || address & 0xfff != 0 {
            return Err(AxError::InvalidInput);
        }
        let (vcpu, _) = aia
            .imsic_addrs
            .iter()
            .find(|(_, addr)| **addr == address)
            .ok_or(AxError::InvalidInput)?;
        aia.files.get(vcpu).ok_or(AxError::InvalidInput)?.clone()
    };
    if !file.inject(data as usize)? {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

pub(super) fn handle_exit(
    vm_file: api::ControlFileId,
    vcpu: &axvm::AxVCpuRef,
    exit: &axvcpu::AxVCpuExitReason,
) -> AxResult<bool> {
    use axvcpu::AxVCpuExitReason;
    let (address, width, data) = match exit {
        AxVCpuExitReason::MmioRead { addr, width, .. } => (addr.as_usize() as u64, *width, None),
        AxVCpuExitReason::MmioWrite { addr, width, data } => {
            (addr.as_usize() as u64, *width, Some(*data as u32))
        }
        _ => return Ok(false),
    };
    let Ok(shared) = state(vm_file) else {
        return Ok(false);
    };
    let mut aia = shared.lock();
    if !aia.initialized {
        return Ok(false);
    }
    let mut result = 0;
    let mut pending = Vec::new();
    if let Some(base) = aia.aplic_addr
        && address >= base
        && address < base + 0x4000
    {
        if super::util::access_width_bytes(width) != 4 || address & 3 != 0 {
            return Err(AxError::InvalidInput);
        }
        let offset = (address - base) as usize;
        if let Some(value) = data {
            aia.aplic.write(offset, value);
            pending = drain(&mut aia);
        } else {
            result = aia.aplic.read(offset) as usize;
        }
    } else if let Some((hart, base)) = aia
        .imsic_addrs
        .iter()
        .find(|(_, base)| address >= **base && address < **base + 0x1000)
    {
        if super::util::access_width_bytes(width) != 4 || !matches!(address - base, 0 | 4) {
            return Err(AxError::InvalidInput);
        }
        if let Some(value) = data {
            let id = if address - base == 4 {
                value.swap_bytes()
            } else {
                value
            };
            pending.push((
                aia.files.get(hart).ok_or(AxError::InvalidInput)?.clone(),
                id as usize,
            ));
        }
    } else {
        return Ok(false);
    }
    drop(aia);
    dispatch(pending)?;
    if let AxVCpuExitReason::MmioRead {
        reg, signed_ext, ..
    } = exit
    {
        vcpu.set_gpr(
            *reg,
            if *signed_ext {
                result as u32 as i32 as isize as usize
            } else {
                result
            },
        );
    }
    Ok(true)
}

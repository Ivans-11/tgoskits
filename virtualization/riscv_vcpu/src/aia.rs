//! Shared IMSIC backing for static and control VMs, with optional core-managed hardware guest files.
use alloc::sync::Arc;

use ax_errno::AxResult;
use ax_kspin::SpinNoIrq;
use riscv_aia::Imsic;

#[cfg(feature = "aia-hw")]
mod hardware;
#[cfg(feature = "aia-hw")]
pub use hardware::init_on_cpu as init_hardware_on_cpu;

/// Producers and the entry path serialize publication of pending state and CPU.
pub struct ImsicFile {
    /// Guest-visible interrupt state.
    pub state: SpinNoIrq<FileState>,
}

/// State protected by the interrupt-file lock.
pub struct FileState {
    /// Software interrupt registers.
    pub imsic: Imsic,
    cpu: Option<usize>,
    #[cfg(feature = "aia-hw")]
    hardware: Option<hardware::GuestFile>,
    #[cfg(feature = "aia-hw")]
    software_users: usize,
    #[cfg(feature = "aia-hw")]
    allow_hardware: bool,
    #[cfg(feature = "aia-hw")]
    ids: usize,
}

impl ImsicFile {
    /// Creates a software interrupt file with the requested maximum identity.
    pub fn new(ids: usize, _allow_hardware: bool) -> Option<Arc<Self>> {
        Some(Arc::new(Self {
            state: SpinNoIrq::new(FileState {
                imsic: Imsic::new(ids)?,
                cpu: None,
                #[cfg(feature = "aia-hw")]
                hardware: None,
                #[cfg(feature = "aia-hw")]
                software_users: 0,
                #[cfg(feature = "aia-hw")]
                allow_hardware: _allow_hardware,
                #[cfg(feature = "aia-hw")]
                ids,
            }),
        }))
    }

    /// Updates registers or pending bits, then interrupts an active guest.
    pub fn update<T>(&self, f: impl FnOnce(&mut Imsic) -> T) -> AxResult<T> {
        // Block hardware re-entry before requesting an exit. The synchronous
        // kick runs only after the target has reclaimed its hardware state.
        // Never hold the file lock across a kick.
        #[cfg(feature = "aia-hw")]
        {
            let cpu = {
                let mut file = self.state.lock();
                file.software_users += 1;
                file.hardware.as_ref().and(file.cpu)
            };
            if let Some(cpu) = cpu
                && let Err(error) = axvisor_api::arch::kick_cpu_wait(cpu)
            {
                self.state.lock().software_users -= 1;
                return Err(error);
            }
        }
        let (result, cpu) = {
            let mut file = self.state.lock();
            let result = f(&mut file.imsic);
            #[cfg(feature = "aia-hw")]
            {
                file.software_users -= 1;
            }
            (result, file.cpu)
        };
        if let Some(cpu) = cpu {
            axvisor_api::arch::kick_cpu_wait(cpu)?;
        }
        Ok(result)
    }

    /// Injects an MSI directly into a running hardware file, or queues it in
    /// software and kicks the active CPU. Publication and reclamation share
    /// this lock, so a producer cannot write to a released or migrated file.
    pub fn inject(&self, id: usize) -> AxResult<bool> {
        let (valid, cpu) = {
            let mut file = self.state.lock();
            #[cfg(feature = "aia-hw")]
            if let Some(hardware) = &file.hardware {
                if id == 0 || id > file.ids {
                    return Ok(false);
                }
                // SAFETY: this reserved mapping stays valid for the core's lifetime.
                unsafe {
                    core::arch::asm!("fence iorw, iorw");
                    (hardware.address as *mut u32).write_volatile(id as u32);
                    core::arch::asm!("fence iorw, iorw");
                }
                return Ok(true);
            }
            (file.imsic.inject(id), file.cpu)
        };
        if valid && let Some(cpu) = cpu {
            axvisor_api::arch::kick_cpu_wait(cpu)?;
        }
        Ok(valid)
    }
}

/// Architectural state belonging to one virtual CPU.
pub struct Context {
    /// Shared with remote interrupt producers.
    pub file: Arc<ImsicFile>,
    /// Whether the host implements Smstateen.
    pub stateen: bool,
    selector: usize,
}

/// Saved host registers, restored before interrupts are reenabled.
pub(crate) struct HostState {
    selector: usize,
    control: usize,
    stateen: usize,
    #[cfg(feature = "m-mode")]
    machine_stateen: usize,
    #[cfg(feature = "aia-hw")]
    hstatus: usize,
    /// Guest interrupt-file selector, zero for software emulation.
    pub(crate) vgein: usize,
}

impl Context {
    /// Attaches a file before the first entry of a vCPU.
    pub fn new(file: Arc<ImsicFile>, stateen: bool) -> Self {
        Self {
            file,
            stateen,
            selector: 0,
        }
    }

    /// # Safety
    /// Requires Ssaia support, disabled local interrupts and a bound vCPU.
    pub(crate) unsafe fn enter(&mut self) -> HostState {
        let mut saved = HostState {
            selector: 0,
            control: 0,
            stateen: 0,
            #[cfg(feature = "m-mode")]
            machine_stateen: 0,
            #[cfg(feature = "aia-hw")]
            hstatus: 0,
            vgein: 0,
        };
        unsafe {
            core::arch::asm!("csrr {old}, 0x250", old=out(reg) saved.selector);
            core::arch::asm!("csrr {old}, 0x609", old=out(reg) saved.control);
            if self.stateen {
                #[cfg(feature = "m-mode")]
                {
                    // M-mode hosts do not inherit OpenSBI's lower-privilege
                    // permission for IMSIC, AIA and indirect CSR selection.
                    core::arch::asm!("csrr {old}, 0x30c", old=out(reg) saved.machine_stateen);
                    core::arch::asm!("csrw 0x30c, {new}", new=in(reg)
                        (saved.machine_stateen | (7usize << 58) | (1usize << 63)));
                }
                core::arch::asm!("csrr {old}, 0x60c", old=out(reg) saved.stateen);
                core::arch::asm!("csrw 0x60c, {new}", new=in(reg) (saved.stateen | (7usize << 58)));
            }
            #[cfg(feature = "aia-hw")]
            core::arch::asm!("csrr {old}, hstatus", old=out(reg) saved.hstatus);
        }
        let pending = {
            let mut file = self.file.state.lock();
            #[cfg(feature = "aia-hw")]
            if file.allow_hardware
                && file.software_users == 0
                && let Some(hardware) = hardware::acquire()
            {
                if hardware.ids >= file.ids && (1..64).contains(&hardware.index) {
                    saved.vgein = hardware.index;
                    unsafe {
                        core::arch::asm!("csrw hstatus, {value}", value=in(reg)
                            ((saved.hstatus & !(63 << 12)) | (hardware.index << 12)));
                        hardware_load(&file.imsic, file.ids, hardware.ids);
                    }
                    file.hardware = Some(hardware);
                } else {
                    hardware::release(hardware);
                }
            }
            file.cpu = Some(axvisor_api::arch::current_cpu_id());
            saved.vgein == 0 && file.imsic.external_pending()
        };
        unsafe {
            core::arch::asm!("csrw 0x250, {new}", new=in(reg) self.selector);
            core::arch::asm!("csrw 0x609, {new}", new=in(reg)
                if saved.vgein == 0 { (9usize << 16) | usize::from(pending) } else { 0 });
            if pending {
                core::arch::asm!("csrs hvip, {bit}", bit=in(reg) 1usize << 10);
            } else {
                core::arch::asm!("csrc hvip, {bit}", bit=in(reg) 1usize << 10);
            }
        }
        saved
    }

    /// # Safety
    /// Must pair with `enter` on the same CPU before enabling interrupts.
    pub(crate) unsafe fn leave(&mut self, saved: HostState) {
        unsafe {
            core::arch::asm!("csrr {new}, 0x250", new=out(reg) self.selector);
        }
        {
            let mut file = self.file.state.lock();
            file.cpu = None;
            #[cfg(feature = "aia-hw")]
            if let Some(hardware) = file.hardware.take() {
                // The lock excludes all MSI producers until after the lease
                // is returned. Drain MMIO before reading pending state.
                let ids = file.ids;
                unsafe { hardware_save(&mut file.imsic, ids, hardware.ids) };
                hardware::release(hardware);
            }
        }
        unsafe {
            #[cfg(feature = "aia-hw")]
            core::arch::asm!("csrw hstatus, {old}", old=in(reg) saved.hstatus);
            core::arch::asm!("csrw 0x250, {old}", old=in(reg) saved.selector);
            core::arch::asm!("csrw 0x609, {old}", old=in(reg) saved.control);
            if self.stateen {
                core::arch::asm!("csrw 0x60c, {old}", old=in(reg) saved.stateen);
                #[cfg(feature = "m-mode")]
                core::arch::asm!("csrw 0x30c, {old}", old=in(reg) saved.machine_stateen);
            }
        }
    }

    /// Emulates IMSIC CSR accesses with Zicsr read/modify/write semantics.
    pub(crate) fn csr(
        &mut self,
        csr: u16,
        funct3: u8,
        source: usize,
        write: bool,
    ) -> Option<usize> {
        let mut file = self.file.state.lock();
        let old = match csr {
            0x150 => self.selector,
            0x151 => file.imsic.read(self.selector)?,
            0x15c => file.imsic.topei(false),
            _ => return None,
        };
        if write {
            let value = match funct3 & 3 {
                1 => source,
                2 => old | source,
                3 => old & !source,
                _ => return None,
            };
            match csr {
                0x150 => self.selector = value & 0x1ff,
                0x151 => {
                    if !file.imsic.write(self.selector, value) {
                        return None;
                    }
                }
                0x15c => {
                    file.imsic.topei(true);
                }
                _ => unreachable!(),
            }
        }
        Some(old)
    }
}

#[cfg(feature = "aia-hw")]
unsafe fn hardware_read(selector: usize) -> usize {
    let value;
    unsafe {
        core::arch::asm!("csrw 0x250, {selector}", "csrr {value}, 0x251",
            selector=in(reg) selector, value=out(reg) value);
    }
    value
}

#[cfg(feature = "aia-hw")]
unsafe fn hardware_write(selector: usize, value: usize) {
    unsafe {
        core::arch::asm!("csrw 0x250, {selector}", "csrw 0x251, {value}",
            selector=in(reg) selector, value=in(reg) value);
    }
}

#[cfg(feature = "aia-hw")]
unsafe fn hardware_load(imsic: &Imsic, ids: usize, hardware_ids: usize) {
    unsafe {
        hardware_write(0x70, 0);
        for word in 0..=hardware_ids / 64 {
            for base in [0x80, 0xc0] {
                let selector = base + word * 2;
                hardware_write(
                    selector,
                    if word <= ids / 64 {
                        imsic.read(selector).unwrap()
                    } else {
                        0
                    },
                );
            }
        }
        hardware_write(0x72, imsic.read(0x72).unwrap());
        hardware_write(0x70, imsic.read(0x70).unwrap());
        core::arch::asm!("fence iorw, iorw");
    }
}

#[cfg(feature = "aia-hw")]
unsafe fn hardware_save(imsic: &mut Imsic, ids: usize, hardware_ids: usize) {
    unsafe {
        core::arch::asm!("fence iorw, iorw");
        imsic.write(0x70, hardware_read(0x70));
        imsic.write(0x72, hardware_read(0x72));
        hardware_write(0x70, 0);
        for word in 0..=hardware_ids / 64 {
            for base in [0x80, 0xc0] {
                let selector = base + word * 2;
                if word <= ids / 64 {
                    imsic.write(selector, hardware_read(selector));
                }
                hardware_write(selector, 0);
            }
        }
        hardware_write(0x72, 0);
        core::arch::asm!("fence iorw, iorw");
    }
}

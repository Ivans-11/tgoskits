// SPDX-License-Identifier: Apache-2.0

//! KVM register layouts and compatibility rules. Backends only see architectural IDs.

use ax_errno::{AxError, AxResult};
#[cfg(target_arch = "x86_64")]
use axvcpu::x86_regs as reg;
use axvcpu::{AxArchVCpu, AxVCpu};
#[cfg(target_arch = "x86_64")]
use kvm_uapi::x86::{KvmDebugregs, KvmDtable, KvmRegs, KvmSegment, KvmSregs};

pub(super) trait KvmRegisters {
    fn get_kvm_regs(&self, _buf: &mut [u8]) -> AxResult {
        Err(AxError::Unsupported)
    }
    fn set_kvm_regs(&self, _buf: &[u8]) -> AxResult {
        Err(AxError::Unsupported)
    }
    fn get_kvm_sregs(&self, _buf: &mut [u8]) -> AxResult {
        Err(AxError::Unsupported)
    }
    fn set_kvm_sregs(&self, _buf: &[u8]) -> AxResult {
        Err(AxError::Unsupported)
    }
    fn get_kvm_debugregs(&self, _buf: &mut [u8]) -> AxResult {
        Err(AxError::Unsupported)
    }
    fn set_kvm_debugregs(&self, _buf: &[u8]) -> AxResult {
        Err(AxError::Unsupported)
    }
}

#[cfg(not(target_arch = "x86_64"))]
impl<A: AxArchVCpu> KvmRegisters for AxVCpu<A> {}

#[cfg(target_arch = "x86_64")]
fn invalid(_: kvm_uapi::KvmUapiError) -> AxError {
    AxError::InvalidInput
}

#[cfg(target_arch = "x86_64")]
impl<A: AxArchVCpu> KvmRegisters for AxVCpu<A> {
    fn get_kvm_regs(&self, buf: &mut [u8]) -> AxResult {
        let mut values = [
            (0, 0),
            (1, 0),
            (2, 0),
            (3, 0),
            (4, 0),
            (5, 0),
            (6, 0),
            (7, 0),
            (8, 0),
            (9, 0),
            (10, 0),
            (11, 0),
            (12, 0),
            (13, 0),
            (14, 0),
            (15, 0),
            (reg::RIP, 0),
            (reg::RFLAGS, 0),
        ];
        self.get_arch_regs(&mut values)?;
        KvmRegs {
            rax: values[0].1,
            rcx: values[1].1,
            rdx: values[2].1,
            rbx: values[3].1,
            rsp: values[4].1,
            rbp: values[5].1,
            rsi: values[6].1,
            rdi: values[7].1,
            r8: values[8].1,
            r9: values[9].1,
            r10: values[10].1,
            r11: values[11].1,
            r12: values[12].1,
            r13: values[13].1,
            r14: values[14].1,
            r15: values[15].1,
            rip: values[16].1,
            rflags: values[17].1,
        }
        .encode(buf)
        .map_err(invalid)
    }
    fn set_kvm_regs(&self, buf: &[u8]) -> AxResult {
        let r = KvmRegs::decode(buf).map_err(invalid)?;
        self.set_arch_regs(&[
            (0, r.rax),
            (1, r.rcx),
            (2, r.rdx),
            (3, r.rbx),
            (4, r.rsp),
            (5, r.rbp),
            (6, r.rsi),
            (7, r.rdi),
            (8, r.r8),
            (9, r.r9),
            (10, r.r10),
            (11, r.r11),
            (12, r.r12),
            (13, r.r13),
            (14, r.r14),
            (15, r.r15),
            (reg::RIP, r.rip),
            (reg::RFLAGS, r.rflags),
        ])
    }
    fn get_kvm_sregs(&self, buf: &mut [u8]) -> AxResult {
        let mut v = [
            (reg::CS, 0),
            (reg::CS + reg::BASE, 0),
            (reg::CS + reg::LIMIT, 0),
            (reg::CS + reg::ACCESS, 0),
            (reg::DS, 0),
            (reg::DS + reg::BASE, 0),
            (reg::DS + reg::LIMIT, 0),
            (reg::DS + reg::ACCESS, 0),
            (reg::ES, 0),
            (reg::ES + reg::BASE, 0),
            (reg::ES + reg::LIMIT, 0),
            (reg::ES + reg::ACCESS, 0),
            (reg::FS, 0),
            (reg::FS + reg::BASE, 0),
            (reg::FS + reg::LIMIT, 0),
            (reg::FS + reg::ACCESS, 0),
            (reg::GS, 0),
            (reg::GS + reg::BASE, 0),
            (reg::GS + reg::LIMIT, 0),
            (reg::GS + reg::ACCESS, 0),
            (reg::SS, 0),
            (reg::SS + reg::BASE, 0),
            (reg::SS + reg::LIMIT, 0),
            (reg::SS + reg::ACCESS, 0),
            (reg::TR, 0),
            (reg::TR + reg::BASE, 0),
            (reg::TR + reg::LIMIT, 0),
            (reg::TR + reg::ACCESS, 0),
            (reg::LDTR, 0),
            (reg::LDTR + reg::BASE, 0),
            (reg::LDTR + reg::LIMIT, 0),
            (reg::LDTR + reg::ACCESS, 0),
            (reg::GDTR + reg::BASE, 0),
            (reg::GDTR + reg::LIMIT, 0),
            (reg::IDTR + reg::BASE, 0),
            (reg::IDTR + reg::LIMIT, 0),
            (reg::CR0, 0),
            (reg::CR2, 0),
            (reg::CR3, 0),
            (reg::CR4, 0),
            (reg::EFER, 0),
        ];
        self.get_arch_regs(&mut v)?;
        KvmSregs {
            cs: KvmSegment::from_access_rights(v[0].1 as u16, v[1].1, v[2].1 as u32, v[3].1 as u32),
            ds: KvmSegment::from_access_rights(v[4].1 as u16, v[5].1, v[6].1 as u32, v[7].1 as u32),
            es: KvmSegment::from_access_rights(
                v[8].1 as u16,
                v[9].1,
                v[10].1 as u32,
                v[11].1 as u32,
            ),
            fs: KvmSegment::from_access_rights(
                v[12].1 as u16,
                v[13].1,
                v[14].1 as u32,
                v[15].1 as u32,
            ),
            gs: KvmSegment::from_access_rights(
                v[16].1 as u16,
                v[17].1,
                v[18].1 as u32,
                v[19].1 as u32,
            ),
            ss: KvmSegment::from_access_rights(
                v[20].1 as u16,
                v[21].1,
                v[22].1 as u32,
                v[23].1 as u32,
            ),
            tr: KvmSegment::from_access_rights(
                v[24].1 as u16,
                v[25].1,
                v[26].1 as u32,
                v[27].1 as u32,
            ),
            ldt: KvmSegment::from_access_rights(
                v[28].1 as u16,
                v[29].1,
                v[30].1 as u32,
                v[31].1 as u32,
            ),
            gdt: KvmDtable {
                base: v[32].1,
                limit: v[33].1 as u16,
            },
            idt: KvmDtable {
                base: v[34].1,
                limit: v[35].1 as u16,
            },
            cr0: v[36].1,
            cr2: v[37].1,
            cr3: v[38].1,
            cr4: v[39].1,
            efer: v[40].1,
            // Preserve the fields historically emulated by this frontend.
            cr8: 0,
            apic_base: 0,
            interrupt_bitmap: [0; 4],
        }
        .encode(buf)
        .map_err(invalid)
    }
    fn set_kvm_sregs(&self, buf: &[u8]) -> AxResult {
        let mut s = KvmSregs::decode(buf).map_err(invalid)?;
        // KVM accepts an all-zero LDT as an absent segment.
        if s.ldt == KvmSegment::default() {
            s.ldt.unusable = 1;
        }
        self.set_arch_regs(&[
            (reg::CR2, s.cr2),
            (reg::CS, s.cs.selector as u64),
            (reg::CS + reg::BASE, s.cs.base),
            (reg::CS + reg::LIMIT, s.cs.limit as u64),
            (reg::CS + reg::ACCESS, s.cs.access_rights() as u64),
            (reg::DS, s.ds.selector as u64),
            (reg::DS + reg::BASE, s.ds.base),
            (reg::DS + reg::LIMIT, s.ds.limit as u64),
            (reg::DS + reg::ACCESS, s.ds.access_rights() as u64),
            (reg::ES, s.es.selector as u64),
            (reg::ES + reg::BASE, s.es.base),
            (reg::ES + reg::LIMIT, s.es.limit as u64),
            (reg::ES + reg::ACCESS, s.es.access_rights() as u64),
            (reg::FS, s.fs.selector as u64),
            (reg::FS + reg::BASE, s.fs.base),
            (reg::FS + reg::LIMIT, s.fs.limit as u64),
            (reg::FS + reg::ACCESS, s.fs.access_rights() as u64),
            (reg::GS, s.gs.selector as u64),
            (reg::GS + reg::BASE, s.gs.base),
            (reg::GS + reg::LIMIT, s.gs.limit as u64),
            (reg::GS + reg::ACCESS, s.gs.access_rights() as u64),
            (reg::SS, s.ss.selector as u64),
            (reg::SS + reg::BASE, s.ss.base),
            (reg::SS + reg::LIMIT, s.ss.limit as u64),
            (reg::SS + reg::ACCESS, s.ss.access_rights() as u64),
            (reg::TR, s.tr.selector as u64),
            (reg::TR + reg::BASE, s.tr.base),
            (reg::TR + reg::LIMIT, s.tr.limit as u64),
            (reg::TR + reg::ACCESS, s.tr.access_rights() as u64),
            (reg::LDTR, s.ldt.selector as u64),
            (reg::LDTR + reg::BASE, s.ldt.base),
            (reg::LDTR + reg::LIMIT, s.ldt.limit as u64),
            (reg::LDTR + reg::ACCESS, s.ldt.access_rights() as u64),
            (reg::GDTR + reg::BASE, s.gdt.base),
            (reg::GDTR + reg::LIMIT, s.gdt.limit as u64),
            (reg::IDTR + reg::BASE, s.idt.base),
            (reg::IDTR + reg::LIMIT, s.idt.limit as u64),
            (reg::CR0, s.cr0),
            (reg::CR3, s.cr3),
            (reg::CR4, s.cr4),
            (reg::EFER, s.efer),
        ])
    }
    fn get_kvm_debugregs(&self, buf: &mut [u8]) -> AxResult {
        let mut v = [
            (reg::DR0, 0),
            (reg::DR1, 0),
            (reg::DR2, 0),
            (reg::DR3, 0),
            (reg::DR6, 0),
            (reg::DR7, 0),
        ];
        self.get_arch_regs(&mut v)?;
        KvmDebugregs {
            db: [v[0].1, v[1].1, v[2].1, v[3].1],
            dr6: v[4].1,
            dr7: v[5].1,
            flags: 0,
            reserved: [0; 9],
        }
        .encode(buf)
        .map_err(invalid)
    }
    fn set_kvm_debugregs(&self, buf: &[u8]) -> AxResult {
        let r = KvmDebugregs::decode(buf).map_err(invalid)?;
        if r.flags != 0 || r.reserved.iter().any(|v| *v != 0) {
            return Err(AxError::InvalidInput);
        }
        self.set_arch_regs(&[
            (reg::DR0, r.db[0]),
            (reg::DR1, r.db[1]),
            (reg::DR2, r.db[2]),
            (reg::DR3, r.db[3]),
            (reg::DR6, r.dr6),
            (reg::DR7, r.dr7),
        ])
    }
}

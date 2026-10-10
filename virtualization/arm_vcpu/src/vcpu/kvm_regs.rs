// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ax_errno::{AxError, AxResult};
use kvm_uapi::arm64::*;

use super::Aarch64VCpu;

const CORE_X30: u64 = KVM_REG_ARM64_CORE_X0 + 60;

const ARM64_REG_IDS: [u64; 36] = {
    let mut ids = [0; 36];
    let mut index = 0;
    while index < 31 {
        ids[index] = KVM_REG_ARM64_CORE_X0 + (index as u64) * 2;
        index += 1;
    }
    ids[31] = KVM_REG_ARM64_CORE_SP_EL0;
    ids[32] = KVM_REG_ARM64_CORE_PC;
    ids[33] = KVM_REG_ARM64_CORE_PSTATE;
    ids[34] = KVM_REG_ARM64_MPIDR_EL1;
    ids[35] = KVM_REG_ARM64_SCTLR_EL1;
    ids
};

impl Aarch64VCpu {
    pub(super) fn get_kvm_arch_reg(&self, reg_id: u64) -> AxResult<u64> {
        match reg_id {
            KVM_REG_ARM64_CORE_X0..=CORE_X30
                if (reg_id - KVM_REG_ARM64_CORE_X0).is_multiple_of(2) =>
            {
                Ok(self.ctx.gpr[((reg_id - KVM_REG_ARM64_CORE_X0) / 2) as usize])
            }
            KVM_REG_ARM64_CORE_SP_EL0 => Ok(self.ctx.sp_el0),
            KVM_REG_ARM64_CORE_PC => Ok(self.ctx.elr),
            KVM_REG_ARM64_CORE_PSTATE => Ok(self.ctx.spsr),
            KVM_REG_ARM64_CORE_SP_EL1 => Ok(self.guest_system_regs.sp_el1()),
            KVM_REG_ARM64_MPIDR_EL1 => Ok(self.mpidr),
            KVM_REG_ARM64_SCTLR_EL1 => Ok(self.guest_system_regs.sctlr_el1 as u64),
            _ => Err(AxError::Unsupported),
        }
    }

    pub(super) fn kvm_arch_reg_ids(&self) -> &'static [u64] {
        &ARM64_REG_IDS
    }

    pub(super) fn set_kvm_arch_reg(&mut self, reg_id: u64, value: u64) -> AxResult {
        match reg_id {
            KVM_REG_ARM64_CORE_X0..=CORE_X30
                if (reg_id - KVM_REG_ARM64_CORE_X0).is_multiple_of(2) =>
            {
                self.ctx.gpr[((reg_id - KVM_REG_ARM64_CORE_X0) / 2) as usize] = value;
            }
            KVM_REG_ARM64_CORE_SP_EL0 => self.ctx.sp_el0 = value,
            KVM_REG_ARM64_CORE_PC => self.ctx.elr = value,
            KVM_REG_ARM64_CORE_PSTATE => self.ctx.spsr = value,
            KVM_REG_ARM64_CORE_SP_EL1 => self.guest_system_regs.set_sp_el1(value),
            KVM_REG_ARM64_SCTLR_EL1 => self.guest_system_regs.sctlr_el1 = value as u32,
            KVM_REG_ARM64_MPIDR_EL1 if value == self.mpidr => {}
            KVM_REG_ARM64_MPIDR_EL1 => return Err(AxError::InvalidInput),
            _ => return Err(AxError::Unsupported),
        }
        Ok(())
    }
}

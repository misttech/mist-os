// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::XSAVE_AREA_SIZE as X64_XSAVE_AREA_SIZE;

#[cfg(target_arch = "x86_64")]
pub use x86_64::SUPPORTED_XSAVE_FEATURES as X64_SUPPORTED_XSAVE_FEATURES;

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[cfg(target_arch = "riscv64")]
pub mod riscv64;

#[derive(Clone, Copy, Default)]
pub struct ExtendedPstateState {
    #[cfg(target_arch = "x86_64")]
    state: x86_64::State,

    #[cfg(target_arch = "aarch64")]
    state: aarch64::State,

    #[cfg(target_arch = "riscv64")]
    state: riscv64::State,
}

impl ExtendedPstateState {
    /// Runs the provided function with the saved extended processor state.  The
    /// provided function must be written carefully to avoid inadvertently using
    /// any extended state itself such as vector or floating point registers.
    ///
    /// # Safety
    ///
    /// |f| must not use any extended processor state.
    pub unsafe fn run_with_saved_state<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        self.restore();
        let r = f();
        self.save();
        r
    }

    #[cfg(target_arch = "x86_64")]
    pub fn with_strategy(strategy: x86_64::Strategy) -> Self {
        Self { state: x86_64::State::with_strategy(strategy) }
    }

    /// This saves the current extended processor state to this state object.
    #[inline(always)]
    fn save(&mut self) {
        self.state.save()
    }

    #[inline(always)]
    /// This restores the extended processor state saved in this object into the processor's state
    /// registers.
    ///
    /// Safety: This clobbers the current vector register, floating point register, and floating
    /// point status and control register state including callee-saved registers. This should be
    /// used in conjunction with save() to switch to an alternate extended processor state.
    unsafe fn restore(&self) {
        self.state.restore()
    }

    pub fn reset(&mut self) {
        self.state.reset()
    }

    #[cfg(target_arch = "aarch64")]
    pub fn get_arm64_qregs(&self) -> &[u128; 32] {
        &self.state.q
    }

    #[cfg(target_arch = "aarch64")]
    pub fn get_arm64_fpsr(&self) -> u32 {
        self.state.fpsr
    }

    #[cfg(target_arch = "aarch64")]
    pub fn get_arm64_fpcr(&self) -> u32 {
        self.state.fpcr
    }

    #[cfg(target_arch = "aarch64")]
    pub fn set_arm64_state(&mut self, qregs: &[u128; 32], fpsr: u32, fpcr: u32) {
        self.state.q = *qregs;
        self.state.fpsr = fpsr;
        self.state.fpcr = fpcr;
    }

    #[cfg(target_arch = "riscv64")]
    pub fn get_riscv64_state(&self) -> &riscv64::State {
        &self.state
    }

    #[cfg(target_arch = "riscv64")]
    pub fn get_riscv64_state_mut(&mut self) -> &mut riscv64::State {
        &mut self.state
    }

    #[cfg(target_arch = "x86_64")]
    pub fn get_x64_xsave_area(&self) -> [u8; X64_XSAVE_AREA_SIZE] {
        unsafe { std::mem::transmute(self.state.buffer) }
    }

    #[cfg(target_arch = "x86_64")]
    pub fn set_x64_xsave_area(&mut self, xsave_area: [u8; X64_XSAVE_AREA_SIZE]) {
        self.state.set_xsave_area(xsave_area);
    }
}

#[no_mangle]
unsafe extern "C" fn restore_extended_pstate(state_addr: usize) {
    let state = state_addr as *mut ExtendedPstateState;
    (&*state).restore()
}

#[no_mangle]
unsafe extern "C" fn save_extended_pstate(state_addr: usize) {
    let state = state_addr as *mut ExtendedPstateState;
    (&mut *state).save()
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    fn extended_pstate_state_lifecycle() {
        let mut state = ExtendedPstateState::default();
        unsafe {
            state.save();
            state.restore();
        }
    }
}

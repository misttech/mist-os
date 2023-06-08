// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[cfg(target_arch = "riscv64")]
mod riscv64;

#[derive(Default)]
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

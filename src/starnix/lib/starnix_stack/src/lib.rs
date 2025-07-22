// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zx;

unsafe extern "C" {
    fn __get_unsafe_stack_ptr() -> usize;
}

/// Compute the safe stack pointer inside this function.
///
/// This must never be inlined to ensure the stack pointer is not the one for the previous
/// function.
#[inline(never)]
fn get_safe_stack_ptr() -> usize {
    let mut sp: usize;
    unsafe {
        #[cfg(target_arch = "x86_64")]
        std::arch::asm!(
            "mov {0}, rsp",
            out(reg) sp,
            options(nostack, nomem)
        );
        #[cfg(target_arch = "aarch64")]
        std::arch::asm!(
            "mov {0}, sp",
            out(reg) sp,
            options(nostack, nomem)
        );
        #[cfg(target_arch = "riscv64")]
        std::arch::asm!(
            "mv {0}, sp",
            out(reg) sp,
            options(nostack, nomem)
        );
    }
    sp
}

/// Compute the safe stack pointer inside this function.
///
/// This can be inlined because it calls into C, so the underlying call will never be inlined.
fn get_unsafe_stack_ptr() -> usize {
    unsafe { __get_unsafe_stack_ptr() }
}

pub fn clean_stack() {
    // Stacks are 2Mo, clean 1Mo.
    const CLEAN_SIZE: usize = 0x100000;
    let page_size: usize = zx::system_get_page_size() as usize;
    let stack_ptr_factories = [get_unsafe_stack_ptr, get_safe_stack_ptr];
    for factory in stack_ptr_factories {
        // Compute the pointers to the stack as a function call from this function to ensure the
        // pointed memory is unused.
        let v = factory();
        // ptr point to the last used byte on the stack, so move it to the first unused byte.
        let v = v - 1;
        let max_addr = v - (v % page_size);
        let start_addr = max_addr - CLEAN_SIZE;
        let _ =
            fuchsia_runtime::vmar_root_self().op_range(zx::VmarOp::ZERO, start_addr, CLEAN_SIZE);
    }
}

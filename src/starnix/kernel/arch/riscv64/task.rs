// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::task::PageFaultExceptionReport;
use crate::types::signals::Signal;

// See "4.1.8 Supervisor Cause Register" in "The RISC-V Instruction Set Manual, Volume II:
// Privileged Architecture".
const RISCV64_EXCEPTION_STORE_PAGE_FAULT: u64 = 15;

pub fn decode_page_fault_exception_report(
    report: &zx::sys::zx_exception_report_t,
) -> PageFaultExceptionReport {
    // Safety: The union contains riscv64 data when building for the riscv64 architecture.
    let riscv_data = unsafe { report.context.arch.riscv_64 };
    let faulting_address = riscv_data.tval;

    // TODO(fxbug.dev/128554): Is there a way to distinguish access and page-not-present faults?
    let not_present = true;

    let is_write = riscv_data.cause == RISCV64_EXCEPTION_STORE_PAGE_FAULT;

    PageFaultExceptionReport { faulting_address, not_present, is_write }
}

pub fn get_signal_for_general_exception(
    _context: &zx::sys::zx_exception_context_t,
) -> Option<Signal> {
    // TODO(fxbug.dev/128554) Return SIGFPE for FP exceptions.
    None
}

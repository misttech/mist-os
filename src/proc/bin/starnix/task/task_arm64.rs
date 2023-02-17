// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::task::PageFaultExceptionReport;

pub fn decode_page_fault_exception_report(
    report: &zx::sys::zx_exception_report_t,
) -> PageFaultExceptionReport {
    // Safety: The union contains arm64 data when building for the arm64 architecture.
    let arm64_data = unsafe { report.context.arch.arm_64 };
    let faulting_address = arm64_data.far;

    // Exception Class is bits 26-31 (inclusive).
    let ec = (arm64_data.esr >> 26) & 0b111111;
    let not_present = ec == 0b100100 || ec == 0b100101; // Data abort exceptions.

    // Note that the Zircon exception handler arm64_data_abort_handler() adds some
    // extra checking of the "cache maintenance" bit which causes it to treat more
    // things as reads. We may need similar handling.
    let is_write = arm64_data.esr & 0b1000000 != 0; // WnR "write not read" = bit 6.

    PageFaultExceptionReport { faulting_address, not_present, is_write }
}

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::task::PageFaultExceptionReport;

pub fn decode_page_fault_exception_report(
    report: &zx::sys::zx_exception_report_t,
) -> PageFaultExceptionReport {
    // Safety: The union contains x86_64 data when building for the x86_64 architecture.
    let x86_64_data = unsafe { report.context.arch.x86_64 };

    // [intel/vol3]: 6.15: Interrupt 14--Page-Fault Exception (#PF)
    let faulting_address = x86_64_data.cr2;
    let not_present = x86_64_data.err_code & 0x01 == 0; // Low bit means "present"
    let is_write = x86_64_data.err_code & 0x02 != 0;

    PageFaultExceptionReport { faulting_address, not_present, is_write }
}

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::PageFaultExceptionReport;
use starnix_uapi::signals::{Signal, SIGFPE};

// See https://developer.arm.com/documentation/ddi0601/2022-03/AArch64-Registers/ESR-EL1--Exception-Syndrome-Register--EL1-
// for details about the values used in this file.

// Returns "Exception Class" from the exception context.
fn get_ec_from_exception_context(context: &zx::sys::zx_exception_context_t) -> u8 {
    // Safety: The union contains arm64 data when building for the arm64 architecture.
    let esr = unsafe { context.arch.arm_64.esr };
    // Exception Class is bits 26-31 (inclusive).
    ((esr >> 26) & 0b111111u32) as u8
}

// Returns "Instruction Specific Syndrome" from the exception context.
fn get_iss_from_exceptoion_context(context: &zx::sys::zx_exception_context_t) -> u32 {
    // Safety: The union contains arm64 data when building for the arm64 architecture.
    let esr = unsafe { context.arch.arm_64.esr };
    // ISS is bits 0-24 (inclusive).
    esr & 0xffffffu32
}

pub fn decode_page_fault_exception_report(
    report: &zx::sys::zx_exception_report_t,
) -> PageFaultExceptionReport {
    // Safety: The union contains arm64 data when building for the arm64 architecture.
    let arm64_data = unsafe { report.context.arch.arm_64 };
    let faulting_address = arm64_data.far;

    let ec = get_ec_from_exception_context(&report.context);
    let iss = get_iss_from_exceptoion_context(&report.context);

    let is_execute = ec == 0b100000 || ec == 0b100001; // Instruction abort exceptions.
    let data_abort = ec == 0b100100 || ec == 0b100101; // Data abort exceptions.

    // Data Fault Status Code or Instruction Fault Status Code (bits [0:5] of ISS).
    let dfsc = iss & 0b111111;

    // Translation faults, level 0-3.
    let not_present =
        (dfsc == 0b000100) || (dfsc == 0b000101) || (dfsc == 0b000110) || (dfsc == 0b000111);

    // Note that the Zircon exception handler arm64_data_abort_handler() adds some
    // extra checking of the "cache maintenance" bit which causes it to treat more
    // things as reads. We may need similar handling.
    let is_write = data_abort && (arm64_data.esr & 0b1000000 != 0); // WnR "write not read" = bit 6.

    PageFaultExceptionReport { faulting_address, not_present, is_write, is_execute }
}

pub fn get_signal_for_general_exception(
    context: &zx::sys::zx_exception_context_t,
) -> Option<Signal> {
    match get_ec_from_exception_context(context) {
        // Floating point exception.
        0b101000 | 0b101100 => Some(SIGFPE),

        _ => None,
    }
}

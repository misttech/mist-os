// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_LOCKUP_DETECTOR_INCLUDE_LIB_LOCKUP_DETECTOR_DIAGNOSTICS_H_
#define ZIRCON_KERNEL_LIB_LOCKUP_DETECTOR_INCLUDE_LIB_LOCKUP_DETECTOR_DIAGNOSTICS_H_

#include <inttypes.h>
#include <stdio.h>

#include <arch/defines.h>
#include <arch/vm.h>
#include <kernel/cpu.h>

#if defined(__aarch64__)
#include <arch/arm64/dap.h>
#include <arch/arm64/mmu.h>
#endif

// Attempt to dump the registers and backtrace of the target |cpu|.
//
// This is a destructive operation and may leave the target CPU is an unknown state.
//
// Errors:
//
//   ZX_ERR_NOT_SUPPORTED - there is no debug facility present to support this.
//
//   arm64-specific:
//     See documented errors of GetBacktraceFromDapState().
//
//   x86-specific:
//     ZX_ERR_TIMED_OUT - timed out trying to get CPU context.
zx_status_t DumpRegistersAndBacktrace(cpu_num_t cpu, FILE* output_target);

// This namespace is logically private to lockup_detector and its tests.
namespace lockup_internal {

enum class FailureSeverity { Oops, Fatal };
void DumpCommonDiagnostics(cpu_num_t cpu, FILE* output_target, FailureSeverity severity);

#if defined(__aarch64__)
// Using the supplied DAP state, obtain a backtrace, taking care to not fault.
//
// This function is exposed in a header file to facilitate testing.  See |DumpRegistersAndBacktrace|
// for the platform and architecture independent interface.
//
// Resets |out_bt| and then fills it in as much as possible.  The backtrace may be truncated if the
// SCS crosses a page boundary.  The contents of |out_bt| are valid even on error.
//
// Errors:
//   ZX_ERR_BAD_STATE - if the CPU is not in kernel mode.
//   ZX_ERR_INVALID_ARGS - if the SCSP pointer is null or unaligned.
//   ZX_ERR_OUT_OF_RANGE - if the stack is outside kernel address space.
//   ZX_ERR_NOT_FOUND - if the stack is not mapped.
zx_status_t GetBacktraceFromDapState(const arm64_dap_processor_state& state, Backtrace& out_bt);
#endif  // __aarch64__

}  // namespace lockup_internal

#endif  // ZIRCON_KERNEL_LIB_LOCKUP_DETECTOR_INCLUDE_LIB_LOCKUP_DETECTOR_DIAGNOSTICS_H_

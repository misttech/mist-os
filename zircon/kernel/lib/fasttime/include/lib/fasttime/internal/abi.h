// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_ABI_H_
#define ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_ABI_H_

// This file describes how the kernel exposes time values to userland.
// This is a PRIVATE UNSTABLE ABI that may change at any time!
// Note that this header is used in both the kernel and in userland in the libfasttime library.
// Therefore, it must be compatible with both the kernel and user header environments.

#include <zircon/time.h>
#include <zircon/types.h>

#include <atomic>

namespace fasttime::internal {

// Many members of this struct are marked const to force folks who initialize the structure to
// explicitly declare values at the time of instantiation. The primary use case for this is the
// test code. Note that all accesses of this structure should still be done with a const reference.
struct TimeValues {
  // A version number to check against the version of libfasttime.
  const uint64_t version;

  // Conversion factor for zx_ticks_get return values to seconds.
  const zx_ticks_t ticks_per_second;

  // Offset for converting from the raw system timer to boot ticks.
  const zx_ticks_t boot_ticks_offset;

  // Offset for converting from the raw system timer to monotonic ticks.
  std::atomic<zx_ticks_t> mono_ticks_offset{};

  // Ratio which relates ticks (zx_ticks_get) to clock monotonic (zx_clock_get_monotonic).
  // Specifically...
  //
  // ClockMono(ticks) = (ticks * N) / D
  //
  const uint32_t ticks_to_time_numerator;
  const uint32_t ticks_to_time_denominator;

  // True if usermode can access ticks.
  const bool usermode_can_access_ticks;

  // Whether the A73 errata mitigation must be used when getting ticks.
  // Should always be false on x86 and RISC-V.
  const bool use_a73_errata_mitigation;

  // Whether (on ARM64) the physical counter should be sampled instead of the
  // virtual counter.
  //
  // Should always be false on x86 and RISC-V.
  const bool use_pct_instead_of_vct;
};

}  // namespace fasttime::internal

// PA_VMO_KERNEL_FILE with this name holds the global instance of the TimeValuesVmo.
static constexpr const char kTimeValuesVmoName[] = "time_values";

#endif  // ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_ABI_H_

// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2008 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_PLATFORM_TIMER_H_
#define ZIRCON_KERNEL_INCLUDE_PLATFORM_TIMER_H_

#include <lib/arch/ticks.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

// API to set/clear a hardware timer that is responsible for calling timer_tick() when it fires
zx_status_t platform_set_oneshot_timer(zx_time_t deadline);
void platform_stop_timer();

// Shutdown the calling CPU's platform timer.
//
// Should be called after |platform_stop_timer|, but before taking the CPU offline.
//
// TODO(maniscalco): Provide a "resume" function so we can suspend/resume.
void platform_shutdown_timer();

void timer_tick(zx_time_t now);

// A bool indicating whether or not user mode has direct access to the registers
// which allow directly observing the tick counter or not.
bool platform_usermode_can_access_tick_registers();

// Reads a platform-specific fixed-rate monotonic counter
// The "raw" form of the counter should give the current counter value (and is
// almost certainly not what you want).  The normal form will give the counter
// value, potentially adjusted by a constant used to make the ticks timeline
// start ticking from 0 when the system boots.
zx_ticks_t platform_current_raw_ticks();
zx_ticks_t platform_current_ticks();

// high-precision timer current_ticks
inline zx_ticks_t current_ticks() { return platform_current_ticks(); }

// Access the platform specific offset from the raw ticks timeline to the ticks
// timeline.  The only current legit uses for this function are when
// initializing the RO data for the VDSO, and when fixing up timer values during
// vmexit on ARM (see arch/arm64/hypervisor/vmexit.cc).
zx_ticks_t platform_get_raw_ticks_to_ticks_offset();

// current time in nanoseconds
zx_time_t current_time();

// high-precision timer ticks per second
zx_ticks_t ticks_per_second();

namespace affine {
class Ratio;  // Fwd decl.
}  // namespace affine

// Setter/getter pair for the ratio which defines the relationship between the
// system's tick counter, and the current_time/clock_monotonic clock.  This gets
// set once by architecture specific platform code, after an appropriate ticks
// source has been selected and characterized.
void platform_set_ticks_to_time_ratio(const affine::Ratio& ticks_to_time);
const affine::Ratio& platform_get_ticks_to_time_ratio();

// Convert a sample taken early on to a proper zx_ticks_t, if possible.
// This returns 0 if early samples are not convertible.
zx_ticks_t platform_convert_early_ticks(arch::EarlyTicks sample);

#endif  // ZIRCON_KERNEL_INCLUDE_PLATFORM_TIMER_H_

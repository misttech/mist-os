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

#include <fbl/enum_bits.h>
#include <ktl/atomic.h>
#include <ktl/type_traits.h>

using zx_boot_time_t = int64_t;

namespace internal {

// Offset between the raw ticks counter and the monotonic ticks timeline.
inline ktl::atomic<uint64_t> mono_ticks_offset{0};

// Offset between the raw ticks counter and the boot ticks timeline.
inline ktl::atomic<uint64_t> boot_ticks_offset{0};

}  // namespace internal

// Sets the initial ticks offset. This offset is used to ensure that both the boot and monotonic
// timelines start at 0.
//
// Must be called with interrupts disabled, before secondary CPUs are booted.
inline void timer_set_initial_ticks_offset(uint64_t offset) {
  internal::mono_ticks_offset.store(offset, ktl::memory_order_relaxed);
  internal::boot_ticks_offset.store(offset, ktl::memory_order_relaxed);
}

// Access the platform specific offset from the raw ticks timeline to the monotonic ticks
// timeline.  The only current legit uses for this function are when
// initializing the RO data for the VDSO, and when fixing up timer values during
// vmexit on ARM (see arch/arm64/hypervisor/vmexit.cc).
inline zx_ticks_t timer_get_mono_ticks_offset() {
  return internal::mono_ticks_offset.load(ktl::memory_order_relaxed);
}

// Adds the given ticks to the existing monotonic ticks offset.
inline void timer_add_mono_ticks_offset(zx_ticks_t additional) {
  internal::mono_ticks_offset.fetch_add(additional, ktl::memory_order_relaxed);
}

// Access the offset from the raw ticks timeline to the boot ticks timeline.
inline zx_ticks_t timer_get_boot_ticks_offset() {
  return internal::boot_ticks_offset.load(ktl::memory_order_relaxed);
}

// API to set/clear a hardware timer that is responsible for calling timer_tick() when it fires
zx_status_t platform_set_oneshot_timer(zx_time_t deadline);
void platform_stop_timer();

// Shutdown the calling CPU's platform timer.
//
// Should be called after |platform_stop_timer|, but before taking the CPU offline.
//
// TODO(maniscalco): Provide a "resume" function so we can suspend/resume.
void platform_shutdown_timer();

void timer_tick();

// A bool indicating whether or not user mode has direct access to the registers
// which allow directly observing the tick counter or not.
bool platform_usermode_can_access_tick_registers();

// Current monotonic time in nanoseconds.
zx_time_t current_time();

// Current boot time in nanoseconds.
zx_boot_time_t current_boot_time();

// High-precision timer ticks per second.
zx_ticks_t ticks_per_second();

namespace affine {
class Ratio;  // Fwd decl.
}  // namespace affine

// Setter/getter pair for the ratio which defines the relationship between the
// system's tick counter, and the current_time/clock_monotonic clock.  This gets
// set once by architecture specific platform code, after an appropriate ticks
// source has been selected and characterized.
void timer_set_ticks_to_time_ratio(const affine::Ratio& ticks_to_time);
const affine::Ratio& timer_get_ticks_to_time_ratio();

// Convert a sample taken early on to a proper zx_ticks_t, if possible.
// This returns 0 if early samples are not convertible.
zx_ticks_t platform_convert_early_ticks(arch::EarlyTicks sample);

// A special version of `current_ticks` which is explicitly synchronized with
// memory loads/stores in an architecture/timer specific way.  For example, when
// using TSC as the ticks reference on an x64 system, there is nothing
// explicitly preventing the actual read of the TSC finishing before or after
// previous/subsequent loads/stores (unless we are talking about a subsequent
// store which has a dependency on the TSC read).
//
// Most of the time, this does not matter, but occasionally it can be an issue.
// For example, some algorithms need to record a timestamp while inside of a
// critical section (either shared or exclusive).  While the implementation of
// the synchronization object typically uses atomics and explicit C++ memory
// order annotations to be sure that (for example) stores to some payload done
// by a thread with exclusive access to the payload "happen-before" any
// subsequent loads from threads with shared access to the payload, accesses to
// the ticks reference on a system is typically not constrained by such
// dependencies.
//
// Without special care, the time observed on the system timer can actually
// occur before/after any loads or stores which are part of the entry/exit
// into/out-of the critical section.
//
// In such situations, timer_current_mono_ticks_synchronized can be used instead
// to order to contain the timer observation.  Users pass (via template args) a
// set of flags which cause appropriate architecture specific barriers to be
// inserted before/after the ticks reference observation in order to contain it.
// Specifically, users can demand that the observation must take place:
//
//  + After any previous loads
//  + After any previous stores
//  + Before any subsequent loads
//  + Before any subsequent stores
//
// or any combination of the above.  Some timer/architecture combinations might
// require even more strict synchronization than was requested, but all should
// provide at least the minimum level of synchronization to satisfy the request.

enum class GetTicksSyncFlag : uint8_t {
  kNone = 0,
  kAfterPreviousLoads = (1 << 0),
  kAfterPreviousStores = (1 << 1),
  kBeforeSubsequentLoads = (1 << 2),
  kBeforeSubsequentStores = (1 << 3),
};

FBL_ENABLE_ENUM_BITS(GetTicksSyncFlag)

// Reads a platform-specific ticks counter.
// This raw counter value is almost certainly not what you want; callers generally want to use the
// monotonic ticks value provided by timer_current_mono_ticks. This ticks value is the raw counter
// adjusted by a constant used to make the ticks timeline start ticking from 0 when the system
// boots.
template <GetTicksSyncFlag Flags>
zx_ticks_t platform_current_raw_ticks_synchronized();
inline zx_ticks_t platform_current_raw_ticks() {
  return platform_current_raw_ticks_synchronized<GetTicksSyncFlag::kNone>();
}

template <GetTicksSyncFlag Flags>
inline zx_ticks_t timer_current_mono_ticks_synchronized() {
  while (true) {
    const zx_ticks_t off1 = internal::mono_ticks_offset.load(ktl::memory_order_relaxed);
    const zx_ticks_t raw_ticks = platform_current_raw_ticks_synchronized<Flags>();
    const zx_ticks_t off2 = internal::mono_ticks_offset.load(ktl::memory_order_relaxed);
    if (off1 == off2) {
      return raw_ticks + off1;
    }
  }
}
inline zx_ticks_t timer_current_mono_ticks() {
  return timer_current_mono_ticks_synchronized<GetTicksSyncFlag::kNone>();
}

template <GetTicksSyncFlag Flags>
inline zx_ticks_t timer_current_boot_ticks_synchronized() {
  while (true) {
    const zx_ticks_t off1 = internal::boot_ticks_offset.load(ktl::memory_order_relaxed);
    const zx_ticks_t raw_ticks = platform_current_raw_ticks_synchronized<Flags>();
    const zx_ticks_t off2 = internal::boot_ticks_offset.load(ktl::memory_order_relaxed);
    if (off1 == off2) {
      return raw_ticks + off1;
    }
  }
}
inline zx_ticks_t timer_current_boot_ticks() {
  return timer_current_boot_ticks_synchronized<GetTicksSyncFlag::kNone>();
}

// The current monotonic time in ticks.
inline zx_ticks_t current_ticks() { return timer_current_mono_ticks(); }

// The current boot time in ticks.
inline zx_ticks_t current_boot_ticks() { return timer_current_boot_ticks(); }

#endif  // ZIRCON_KERNEL_INCLUDE_PLATFORM_TIMER_H_

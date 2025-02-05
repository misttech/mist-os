// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <platform.h>

#include <arch/riscv64/feature.h>
#include <dev/timer.h>
#include <platform/timer.h>

// Setup by start.S
arch::EarlyTicks kernel_entry_ticks;
arch::EarlyTicks kernel_virtual_entry_ticks;

// call through to the pdev timer interface
template <GetTicksSyncFlag Flags>
inline zx_ticks_t platform_current_raw_ticks_synchronized() {
  // TODO(johngro): Research what is required in order to properly sync
  // observations of the riscv system timer against the instruction pipeline and
  // apply any needed barriers here.
  return timer_current_ticks();
}

zx_instant_mono_ticks_t platform_convert_early_ticks(arch::EarlyTicks sample) {
  return sample.time + timer_get_mono_ticks_offset();
}

zx_status_t platform_set_oneshot_timer(zx_ticks_t deadline) {
  return timer_set_oneshot_timer(deadline);
}

void platform_stop_timer() { timer_stop(); }

void platform_shutdown_timer() { timer_shutdown(); }

bool platform_usermode_can_access_tick_registers() {
  // If the cpu claims to have Zicntr support, then it's relatively cheap for user
  // space to access the time CSR via rdtime instruction.
  return gRiscvFeatures[arch::RiscvFeature::kZicntr];
}

// Explicit instantiation of all of the forms of synchronized tick access.
#define EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(flags) \
  template zx_ticks_t                                         \
  platform_current_raw_ticks_synchronized<static_cast<GetTicksSyncFlag>(flags)>()
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(0);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(1);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(2);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(3);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(4);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(5);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(6);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(7);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(8);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(9);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(10);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(11);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(12);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(13);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(14);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(15);
#undef EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED

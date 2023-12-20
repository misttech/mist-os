// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <platform.h>

#include <dev/timer.h>
#include <platform/timer.h>

// Setup by start.S
arch::EarlyTicks kernel_entry_ticks;
arch::EarlyTicks kernel_virtual_entry_ticks;

zx_ticks_t raw_ticks_to_ticks_offset{0};

zx_ticks_t platform_get_raw_ticks_to_ticks_offset() { return raw_ticks_to_ticks_offset; }

zx_ticks_t platform_convert_early_ticks(arch::EarlyTicks sample) {
  return sample.time + raw_ticks_to_ticks_offset;
}

// call through to the pdev timer interface
zx_ticks_t platform_current_ticks() { return timer_current_ticks(); }

zx_status_t platform_set_oneshot_timer(zx_time_t deadline) {
  return timer_set_oneshot_timer(deadline);
}

void platform_stop_timer() { timer_stop(); }

void platform_shutdown_timer() { timer_shutdown(); }

bool platform_usermode_can_access_tick_registers() { return false; }

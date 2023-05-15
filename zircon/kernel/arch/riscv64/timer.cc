// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <inttypes.h>
#include <lib/affine/ratio.h>
#include <lib/arch/intrin.h>
#include <lib/counters.h>
#include <lib/unittest/unittest.h>
#include <lib/zbi-format/driver-config.h>
#include <platform.h>
#include <trace.h>
#include <zircon/types.h>

#include <arch/riscv64/sbi.h>
#include <ktl/atomic.h>
#include <ktl/limits.h>
#include <lk/init.h>
#include <platform/timer.h>

#define LOCAL_TRACE 0

// Setup by start.S
arch::EarlyTicks kernel_entry_ticks;
arch::EarlyTicks kernel_virtual_entry_ticks;

namespace {

uint64_t raw_ticks_to_ticks_offset{0};

template <bool AllowDebugPrint = false>
inline affine::Ratio riscv_generic_timer_compute_conversion_factors(uint32_t cntfrq) {
  affine::Ratio cntpct_to_nsec = {ZX_SEC(1), cntfrq};
  if constexpr (AllowDebugPrint) {
    dprintf(SPEW, "riscv generic timer cntpct_per_nsec: %u/%u\n", cntpct_to_nsec.numerator(),
            cntpct_to_nsec.denominator());
  }
  return cntpct_to_nsec;
}

}  // anonymous namespace

void riscv64_timer_exception() {
  riscv64_csr_clear(RISCV64_CSR_SIE, RISCV64_CSR_SIE_TIE);
  timer_tick(current_time());
}

zx_ticks_t platform_current_ticks() { return riscv64_csr_read(RISCV64_CSR_TIME); }

zx_ticks_t platform_get_raw_ticks_to_ticks_offset() {
  // TODO(fxb/91701): consider the memory order semantics of this load when the
  // time comes.
  return raw_ticks_to_ticks_offset;
}

zx_ticks_t platform_convert_early_ticks(arch::EarlyTicks sample) {
  return sample.time + raw_ticks_to_ticks_offset;
}

zx_status_t platform_set_oneshot_timer(zx_time_t deadline) {
  DEBUG_ASSERT(arch_ints_disabled());

  if (deadline < 0) {
    deadline = 0;
  }

  // enable the timer
  riscv64_csr_set(RISCV64_CSR_SIE, RISCV64_CSR_SIE_TIE);

  // convert interval to ticks
  const affine::Ratio time_to_ticks = platform_get_ticks_to_time_ratio().Inverse();
  const uint64_t ticks = time_to_ticks.Scale(deadline) + 1;
  sbi_set_timer(ticks);

  return ZX_OK;
}

void platform_stop_timer() { riscv64_csr_clear(RISCV64_CSR_SIE, RISCV64_CSR_SIE_TIE); }

void platform_shutdown_timer() {
  DEBUG_ASSERT(arch_ints_disabled());
  riscv64_csr_clear(RISCV64_CSR_SIE, RISCV64_CSR_SIE_TIE);
}

bool platform_usermode_can_access_tick_registers() { return false; }

void riscv_generic_timer_init_early(const zbi_dcfg_riscv_generic_timer_driver_t& config) {
  platform_set_ticks_to_time_ratio(
      riscv_generic_timer_compute_conversion_factors<true>(config.freq_hz));
}

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

#include <arch/riscv64/feature.h>
#include <arch/riscv64/sbi.h>
#include <arch/riscv64/timer.h>
#include <ktl/algorithm.h>
#include <ktl/atomic.h>
#include <ktl/limits.h>
#include <pdev/timer.h>
#include <platform/timer.h>

#define LOCAL_TRACE 0

namespace {

template <bool AllowDebugPrint = false>
inline affine::Ratio riscv_generic_timer_compute_conversion_factors(uint32_t cntfrq) {
  affine::Ratio cntpct_to_nsec = {ZX_SEC(1), cntfrq};
  if constexpr (AllowDebugPrint) {
    dprintf(SPEW, "riscv generic timer cntpct_per_nsec: %u/%u\n", cntpct_to_nsec.numerator(),
            cntpct_to_nsec.denominator());
  }
  return cntpct_to_nsec;
}

zx_ticks_t riscv_sbi_current_ticks() { return riscv64_csr_read(RISCV64_CSR_TIME); }

zx_status_t riscv_sbi_set_oneshot_timer(zx_ticks_t deadline) {
  DEBUG_ASSERT(arch_ints_disabled());

  deadline = ktl::max<zx_ticks_t>(deadline, 0);

  LTRACEF("cpu %u: deadline %#" PRIx64 "\n", arch_curr_cpu_num(), deadline);

  // If sstc feature is present, directly set the compare register instead of
  // making a call to SBI.
  if (gRiscvFeatures[arch::RiscvFeature::kSstc]) {
    riscv64_csr_write(RISCV64_CSR_STIMECMP, deadline);
  } else {
    sbi_set_timer(deadline);
  }

  // Enable the timer interrupt.
  riscv64_csr_set(RISCV64_CSR_SIE, RISCV64_CSR_SIE_STIE);

  return ZX_OK;
}

zx_status_t riscv_sbi_timer_stop() {
  riscv64_csr_clear(RISCV64_CSR_SIE, RISCV64_CSR_SIE_STIE);

  return ZX_OK;
}

zx_status_t riscv_sbi_timer_shutdown() {
  DEBUG_ASSERT(arch_ints_disabled());
  riscv64_csr_clear(RISCV64_CSR_SIE, RISCV64_CSR_SIE_STIE);

  return ZX_OK;
}

const pdev_timer_ops riscv_sbi_timer_ops = {
    .current_ticks = riscv_sbi_current_ticks,
    .set_oneshot_timer = riscv_sbi_set_oneshot_timer,
    .stop = riscv_sbi_timer_stop,
    .shutdown = riscv_sbi_timer_shutdown,
};

}  // anonymous namespace

void riscv64_timer_exception() {
  riscv64_csr_clear(RISCV64_CSR_SIE, RISCV64_CSR_SIE_STIE);
  timer_tick();
}

void riscv_generic_timer_init_early(const zbi_dcfg_riscv_generic_timer_driver_t &config) {
  timer_set_ticks_to_time_ratio(
      riscv_generic_timer_compute_conversion_factors<true>(config.freq_hz));

  // register with pdev layer
  dprintf(INFO, "TIMER: registering SBI timer\n");
  pdev_register_timer(&riscv_sbi_timer_ops);
}

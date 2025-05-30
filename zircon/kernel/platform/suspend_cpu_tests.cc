// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/unittest/unittest.h>
#include <platform.h>
#include <zircon/errors.h>

#include <arch/interrupt.h>
#include <kernel/auto_preempt_disabler.h>
#include <kernel/mp.h>

namespace {

bool basic() {
  BEGIN_TEST;

  if (!platform_supports_suspend_cpu()) {
    printf("platform does not support suspend cpu, skipping\n");
    END_TEST;
  }

  {
    AutoPreemptDisabler apd;
    InterruptDisableGuard irqd;
    const cpu_num_t curr_cpu = arch_curr_cpu_num();

    // Pend an interrupt to ourselves to ensure that platform_suspend_cpu returns quickly.
    mp_interrupt(MP_IPI_TARGET_MASK, cpu_num_to_mask(curr_cpu));

    const zx_status_t status = platform_suspend_cpu();

    // platform_suspend_cpu on CPU-0 may behave differently than on other CPUs.  In particular, it
    // may attempt to enter a suspend state that is only valid if all the other CPUs are also in a
    // suspend state.  As such, the call may fail with ZX_ERR_ACCESS_DENIED or ZX_ERR_INVALID_ARGS
    // in normal operation.
    //
    // TODO(https://fxbug.dev/414456459): If we end up changing platform_suspend_cpu to only request
    // a more-than-this-core suspend state, then we can tighten up this test and ensure that
    // ZX_ERR_ACCESS_DENIED and ZX_ERR_INVALID_ARGS are never returned.
    if (curr_cpu != 0) {
      ASSERT_OK(status);
    } else if (status != ZX_ERR_INVALID_ARGS && status != ZX_ERR_ACCESS_DENIED) {
      ASSERT_OK(status);
    }
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(platform_suspend_cpu_tests)
UNITTEST("basic", basic)
UNITTEST_END_TESTCASE(platform_suspend_cpu_tests, "platform_suspend_cpu_tests",
                      "platform_suspend_cpu tests")

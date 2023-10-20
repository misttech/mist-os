// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/system.h>
#include <stdio.h>
#include <zircon/assert.h>

#include <phys/symbolize.h>

#include "test-main.h"

int TestMain(void*, arch::EarlyTicks) {
  MainSymbolize symbolize("drop-el3-test");

  unsigned int old_el = static_cast<unsigned int>(arch::ArmCurrentEl::Read().el());
  printf("%s: Test started in EL%u\n", symbolize.name(), old_el);

  unsigned int new_el = static_cast<unsigned int>(arch::ArmDropEl3().el());

  if (old_el != 3) {
    ZX_ASSERT_MSG(new_el == old_el, "went from EL%u to EL%u!", old_el, new_el);
    printf("%s: Skipping test, not in EL3\n", symbolize.name());
    return 0;
  }

  ZX_ASSERT_MSG(new_el == 2, "went from EL%u to EL%u!", old_el, new_el);
  return 0;
}

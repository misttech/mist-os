// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>

#include <ktl/move.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

#include "physboot.h"
#include "physload.h"

#include <ktl/enforce.h>

PhysBootTimes gBootTimes;

extern "C" void PhysLoadModuleMain(UartDriver& uart, PhysBootTimes boot_times,
                                   KernelStorage kernel_storage) {
  gBootTimes = boot_times;

  gSymbolize->set_name("physboot");
  debugf("%s: Loading Zircon...\n", gSymbolize->name());

  // Now we're ready for the main physboot logic.
  BootZircon(uart, ktl::move(kernel_storage));
}

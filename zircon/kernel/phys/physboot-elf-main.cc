// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>

#include <phys/elf-image.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

#include "log.h"
#include "physboot.h"
#include "physload.h"

#include <ktl/enforce.h>

PhysBootTimes gBootTimes;

extern "C" void PhysLoadHandoff(Log* log, UartDriver& uart, MainSymbolize* symbolize,
                                const BootOptions* boot_options, memalloc::Pool& allocation_pool,
                                PhysBootTimes boot_times, KernelStorage kernel_storage) {
  // Install the log/stdout first thing.  Note this doesn't use log.SetStdout()
  // because we don't want to stash this module's (uninitialized) stdout to be
  // restored, but keep the one already stashed by physload.
  gLog = log;
  FILE::stdout_ = FILE{log};

  // Install the other global state handed off from physload.
  gSymbolize = symbolize;
  gBootOptions = boot_options;
  Allocation::InitWithPool(allocation_pool);
  gBootTimes = boot_times;

  ArchOnPhysLoadHandoff();

  symbolize->set_name("physboot");
  debugf("%s: Loading Zircon...\n", symbolize->name());

  // Now we're ready for the main physboot logic.
  BootZircon(uart, ktl::move(kernel_storage));
}

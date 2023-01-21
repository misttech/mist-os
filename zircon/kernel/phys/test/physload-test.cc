// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "../physload.h"

#include <lib/boot-options/boot-options.h>
#include <zircon/assert.h>

#include <ktl/string_view.h>
#include <phys/allocation.h>
#include <phys/elf-image.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/symbolize.h>

#include <ktl/enforce.h>

void PhysLoadHandoff(FILE* log, MainSymbolize* symbolize, const BootOptions* boot_options,
                     memalloc::Pool& allocation_pool, PhysBootTimes boot_times,
                     KernelStorage kernel_storage) {
  FILE::stdout_ = *log;
  gSymbolize = symbolize;
  gBootOptions = boot_options;
  Allocation::InitWithPool(allocation_pool);

  ktl::string_view me = symbolize->modules().back()->name();
  ZX_ASSERT_MSG(me == "physload-test-data/physload-test",
                "symbolize->name() \"%s\", last module name \"%.*s\"", symbolize->name(),
                static_cast<int>(me.size()), me.data());

  Allocation::GetPool().PrintMemoryRanges(symbolize->name());

  printf("\n*** Test succeeded ***\n%s\n\n", BOOT_TEST_SUCCESS_STRING);

  abort();
}

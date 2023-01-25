// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <stdio.h>

#include <ktl/array.h>
#include <phys/allocation.h>
#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/main.h>
#include <phys/symbolize.h>
#include <phys/uart.h>

#include "log.h"
#include "physboot.h"

#include <ktl/enforce.h>

PhysBootTimes gBootTimes;

void ZbiMain(void* zbi_ptr, arch::EarlyTicks ticks) {
  gBootTimes.Set(PhysBootTimes::kZbiEntry, ticks);

  MainSymbolize symbolize("physboot");

  InitMemory(zbi_ptr);

  // This marks the interval between handoff from the boot loader (kZbiEntry)
  // and phys environment setup with identity-mapped memory management et al.
  gBootTimes.SampleNow(PhysBootTimes::kPhysSetup);

  // Start collecting the log in memory as well as logging to the console.
  Log log;

  {
    // Prime the log with what would already have been written to the console
    // under kernel.phys.verbose=true (even if it wasn't), but don't send that
    // to the console.
    FILE log_file{&log};
    symbolize.ContextAlways(&log_file);
    Allocation::GetPool().PrintMemoryRanges(symbolize.name(), &log_file);
  }

  // Now mirror all stdout to the log, and write debugf there even if verbose
  // logging to stdout is disabled.
  gLog = &log;
  log.SetStdout();

  auto zbi_header = static_cast<zbi_header_t*>(zbi_ptr);
  auto zbi = zbitl::StorageFromRawHeader<ktl::span<ktl::byte>>(zbi_header);

  // Unpack the compressed KERNEL_STORAGE payload.
  KernelStorage kernel_storage;
  kernel_storage.Init(zbitl::View{zbi});
  kernel_storage.GetTimes(gBootTimes);

  // TODO(mcgrathr): Bloat the binary so the total kernel.zbi size doesn't
  // get too comfortably small while physboot functionality is still growing.
  static const ktl::array<char, 512 * 1024> kPad{1};
  __asm__ volatile("" ::"m"(kPad), "r"(kPad.data()));

  BootZircon(GetUartDriver(), ktl::move(kernel_storage));
}

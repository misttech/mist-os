// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/uart/all.h>
#include <lib/zbitl/view.h>
#include <stdio.h>

#include <limits>

#include <ktl/byte.h>
#include <ktl/span.h>
#include <phys/boot-options.h>
#include <phys/main.h>
#include <phys/stdio.h>
#include <phys/uart.h>

#include <ktl/enforce.h>

void PhysMain(void* zbi, arch::EarlyTicks ticks) {
  // Apply any relocations required to ourself.
  ApplyRelocations();

  // Initially set up stdout to write to nowhere.
  InitStdout();

  static BootOptions boot_options;
  // The global is a pointer just for uniformity between the code in phys and
  // in the kernel proper.
  gBootOptions = &boot_options;

  boot_options.serial = GetUartDriver().uart();
  // Obtain proper UART configuration from ZBI, both cmdline items and uart driver items.
  SetBootOptions(boot_options, zbitl::StorageFromRawHeader(static_cast<const zbi_header_t*>(zbi)));

  // Configure the selected UART.
  //
  // Note we don't do this after parsing ZBI items and before parsing command
  // line options, because if kernel.serial overrode what the ZBI items said,
  // we shouldn't be sending output to the wrong UART in between.
  SetUartConsole(boot_options.serial);

  // Perform any architecture-specific set up.
  ArchSetUp(zbi);

  // Call the real entry point now that it can use printf!  It does not return.
  ZbiMain(zbi, ticks);
}

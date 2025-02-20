// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/uart/all.h>
#include <lib/zbitl/view.h>

#include <ktl/move.h>
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

  // Move default uart into the boot options, so its properly overriden (if any) by the different
  // boot options below.
  //
  // TODO(fxb/42084617): To be removed when driver state handed over switches do `uart::all::Config`
  // as we work to remove `uart()` accessor.
  boot_options.serial = ktl::move(GetUartDriver()).TakeUart();

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

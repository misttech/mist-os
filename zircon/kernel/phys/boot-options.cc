// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/boot-options.h"

#include <lib/boot-options/boot-options.h>
#include <lib/uart/all.h>
#include <lib/uart/sync.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/view.h>

void SetBootOptions(BootOptions& boot_opts, zbitl::ByteView zbi, ktl::string_view legacy_cmdline) {
  {
    // Select UART configuration from a UART driver item in the ZBI.
    zbitl::View view(zbi);
    // The |IoProvider| and |SyncPolicy| choice in the driver below aren't relevant, and the sole
    // purpose of |driver| is performing a match.
    uart::all::KernelDriver<uart::BasicIoProvider, uart::UnsynchronizedPolicy> driver;
    for (auto [header, payload] : view) {
      if (driver.Match(*header, payload.data())) {
        boot_opts.serial = driver.uart();
      }
    }
    view.ignore_error();

    // Select UART configuration from cmdline item in the ZBI.
    for (auto [header, payload] : view) {
      if (header->type == ZBI_TYPE_CMDLINE) {
        boot_opts.SetMany({reinterpret_cast<const char*>(payload.data()), payload.size()});
      }
    }
    view.ignore_error();
  }

  // At last the bootloader provided arguments trumps everything.
  boot_opts.SetMany(legacy_cmdline);
}

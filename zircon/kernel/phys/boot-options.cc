// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "phys/boot-options.h"

#include <lib/boot-options/boot-options.h>
#include <lib/boot-options/word-view.h>
#include <lib/uart/all.h>
#include <lib/uart/sync.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/view.h>

#include <explicit-memory/bytes.h>
#include <ktl/algorithm.h>
#include <ktl/move.h>
#include <ktl/string_view.h>

#include <ktl/enforce.h>

void SetBootOptions(BootOptions& boot_opts, zbitl::ByteView zbi, ktl::string_view legacy_cmdline) {
  {
    // Select UART configuration from a UART driver item in the ZBI.
    zbitl::View view(zbi);
    // The |IoProvider| and |SyncPolicy| choice in the driver below aren't relevant, and the sole
    // purpose of |driver| is performing a match.
    uart::all::KernelDriver<uart::BasicIoProvider, uart::UnsynchronizedPolicy> driver;
    for (auto [header, payload] : view) {
      if (driver.Match(*header, payload.data())) {
        boot_opts.serial = ktl::move(driver).TakeUart();
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

void SetBootOptionsWithoutEntropy(BootOptions& boot_opts, zbitl::ByteView zbi,
                                  ktl::string_view legacy_cmdline) {
  SetBootOptions(boot_opts, zbi, legacy_cmdline);
  // Restore the entropy bits.
  // We only use boot-options parsing for kernel.serial and ignore the rest.
  // But it destructively scrubs the RedactedHex input so we have to undo that.
  if (boot_opts.entropy_mixin.len > 0) {
    // BootOptions already parsed and redacted, so put it back.
    for (auto word : WordView(legacy_cmdline)) {
      constexpr ktl::string_view kPrefix = "kernel.entropy-mixin=";
      if (ktl::starts_with(word, kPrefix)) {
        word.remove_prefix(kPrefix.length());
        memcpy(const_cast<char*>(word.data()), boot_opts.entropy_mixin.hex.data(),
               ktl::min(boot_opts.entropy_mixin.len, word.size()));
        mandatory_memset(boot_opts.entropy_mixin.hex.data(), 0, boot_opts.entropy_mixin.len);
        break;
      }
    }
  }
}

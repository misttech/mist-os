// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_BOOT_OPTIONS_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_BOOT_OPTIONS_H_

#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/string_view.h>

struct BootOptions;

namespace zbitl {
using ByteView = ktl::span<const ktl::byte>;
}

// Sets the given boot-options with the specifications encoded in the given ZBI, as well in an
// additional legacy command-line when relevant.
//
// This function does not explicitly modify global state. It is the responsibility
// of the caller to (re)install boot options as `gBootOptions` and call
// `SetUartConsole(boot_options.serial)`.
//
// |legacy_cmdline| override cmdline items contained in the ZBI.
//
// Given a uart specification provided by multiple sources such as lgeacy uart driver,
// ZBI UART driver item, ZBI cmdline item and legacy command line the priority for determining
// which to use is the following:
//
// (1) Legacy cmdline (e.g. boot loader cmdline)
// (2) ZBI cmdline item.
// (3) ZBI UART driver item.
// (4) Legacy UART driver (e.g. ACPI or devicetree).
//
// It is expected that |boot_opts.serial| is set to legacy UART driver(if any) in order to preserve
// this priority.
void SetBootOptions(BootOptions& boot_opts, zbitl::ByteView zbi,
                    ktl::string_view legacy_cmdline = {});

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_BOOT_OPTIONS_H_

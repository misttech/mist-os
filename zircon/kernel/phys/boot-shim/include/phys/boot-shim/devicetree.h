// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_BOOT_SHIM_INCLUDE_PHYS_BOOT_SHIM_DEVICETREE_H_
#define ZIRCON_KERNEL_PHYS_BOOT_SHIM_INCLUDE_PHYS_BOOT_SHIM_DEVICETREE_H_

#include <lib/devicetree/devicetree.h>
#include <lib/memalloc/range.h>
#include <lib/zbi-format/memory.h>
#include <lib/zbitl/view.h>

#include <ktl/optional.h>
#include <ktl/string_view.h>

// Bootstrapping data from the devicetree blob.
struct DevicetreeBoot {
  // Command line from the the devicetree.
  ktl::string_view cmdline;

  // Ramdisk encoded in the devicetree.
  zbitl::ByteView ramdisk;

  // Devicetree span.
  devicetree::Devicetree fdt;

  // Possible nvram range provided by the bootloader.
  ktl::optional<zbi_nvram_t> nvram;
};

// Instance populated by InitMemory().
extern DevicetreeBoot gDevicetreeBoot;

#endif  // ZIRCON_KERNEL_PHYS_BOOT_SHIM_INCLUDE_PHYS_BOOT_SHIM_DEVICETREE_H_

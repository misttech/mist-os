// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_BOOT_SHIM_INCLUDE_PHYS_BOOT_SHIM_DEVICETREE_H_
#define ZIRCON_KERNEL_PHYS_BOOT_SHIM_INCLUDE_PHYS_BOOT_SHIM_DEVICETREE_H_

#include <lib/boot-options/boot-options.h>
#include <lib/boot-shim/devicetree.h>
#include <lib/devicetree/devicetree.h>
#include <lib/fit/defer.h>
#include <lib/fit/result.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/error-stdio.h>
#include <lib/zbitl/item.h>
#include <lib/zbitl/storage-traits.h>
#include <lib/zbitl/view.h>

#include <fbl/alloc_checker.h>
#include <ktl/span.h>
#include <ktl/string_view.h>
#include <ktl/type_traits.h>
#include <phys/allocation.h>
#include <phys/zbitl-allocation.h>

// Bootstrapping data from the devicetree blob.
struct DevicetreeBoot {
  // Command line from the the devicetree.
  ktl::string_view cmdline;

  // Ramdisk encoded in the devicetree.
  zbitl::ByteView ramdisk;

  // Devicetree span.
  devicetree::Devicetree fdt;
};

// Instance populated by InitMemory(void* devicetree blob).
extern DevicetreeBoot gDevicetreeBoot;

// Initializes |gDevicetreeBoot|, memory and uart.
void InitMemory(void* dtb);

#endif  // ZIRCON_KERNEL_PHYS_BOOT_SHIM_INCLUDE_PHYS_BOOT_SHIM_DEVICETREE_H_

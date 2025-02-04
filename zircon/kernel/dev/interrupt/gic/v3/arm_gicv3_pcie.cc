// Copyright 2018 The Fuchsia Authors
// Copyright (c) 2012-2015 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arm_gicv3_pcie.h"

#include <lib/lazy_init/lazy_init.h>
#include <lib/pci/kpci.h>
#include <lib/zbi-format/driver-config.h>
#include <trace.h>
#include <zircon/types.h>

#include <dev/pcie_bus_driver.h>
#include <dev/pcie_platform.h>
#include <dev/pcie_root.h>
#include <lk/init.h>
#include <pdev/interrupt.h>

namespace {
lazy_init::LazyInit<NoMsiPciePlatformInterface, lazy_init::CheckType::None,
                    lazy_init::Destructor::Disabled>
    g_platform_pcie_support;
}  // anonymous namespace

void arm_gicv3_pcie_init() {
  if (!Pci::KernelPciEnabled()) {
    return;
  }

  // When GICv3 MSI support is added, add a handler to register the deny
  // regions. Add all MMIO regions which contain GIC registers to the
  // system-wide deny list using root_resource_filter_add_deny_region.

  // Initialize the PCI platform, claiming no MSI support
  g_platform_pcie_support.Initialize();

  zx_status_t res = PcieBusDriver::InitializeDriver(g_platform_pcie_support.Get());
  if (res != ZX_OK) {
    TRACEF(
        "Failed to initialize PCI bus driver (res %d). "
        "PCI will be non-functional.\n",
        res);
  }
}

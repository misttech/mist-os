// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/lazy_init/lazy_init.h>
#include <lib/pci/kpci.h>
#include <lib/uart/all.h>
#include <trace.h>

#include <dev/init.h>
#include <dev/pcie_bus_driver.h>
#include <dev/pcie_platform.h>
#include <dev/pcie_root.h>
#include <phys/arch/arch-handoff.h>
#include <platform/pc/debug.h>

namespace {

class PcPciePlatformSupport : public PciePlatformInterface {
 public:
  explicit PcPciePlatformSupport(bool has_msi)
      : PciePlatformInterface(has_msi ? MsiSupportLevel::MSI : MsiSupportLevel::NONE) {}

  zx_status_t AllocMsiBlock(uint requested_irqs, bool can_target_64bit, bool is_msix,
                            msi_block_t* out_block) override {
    return msi_alloc_block(requested_irqs, can_target_64bit, is_msix, out_block);
  }

  void FreeMsiBlock(msi_block_t* block) override { msi_free_block(block); }

  void RegisterMsiHandler(const msi_block_t* block, uint msi_id, int_handler handler,
                          void* ctx) override {
    return msi_register_handler(block, msi_id, handler, ctx);
  }
};

lazy_init::LazyInit<PcPciePlatformSupport, lazy_init::CheckType::None,
                    lazy_init::Destructor::Disabled>
    g_platform_pcie_support;
}  // namespace

void PlatformDriverHandoffEarly(const ArchPhysHandoff& arch_handoff) {}

void PlatformDriverHandoffLate(const ArchPhysHandoff& arch_handoff) {
  // Initialize the PCI platform, claiming MSI support
  g_platform_pcie_support.Initialize(true);

  zx_status_t res = PcieBusDriver::InitializeDriver(g_platform_pcie_support.Get());
  if (res != ZX_OK) {
    TRACEF(
        "Failed to initialize PCI bus driver (res %d).  "
        "PCI will be non-functional.\n",
        res);
  }
}

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BOARD_DRIVERS_CROSVM_CROSVM_H_
#define SRC_DEVICES_BOARD_DRIVERS_CROSVM_CROSVM_H_

#include <fidl/fuchsia.hardware.pci/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/wire.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <fuchsia/hardware/pciroot/c/banjo.h>
#include <fuchsia/hardware/pciroot/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devicetree/visitors/drivers/pci/pci.h>
#include <lib/driver/metadata/cpp/metadata_server.h>
#include <lib/pci/devicetree.h>
#include <lib/pci/pciroot.h>
#include <lib/pci/root_host.h>
#include <lib/zx/resource.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <span>

#include <bind/fuchsia/pci/cpp/bind.h>

namespace board_crosvm {

class Pciroot : public PcirootBase, public ddk::PcirootProtocol<Pciroot> {
 public:
  Pciroot() = delete;
  Pciroot(std::string node_name, PciRootHost* root_host, async_dispatcher_t* dispatcher,
          fdf::ClientEnd<fuchsia_hardware_platform_bus::Iommu> iommu, zx::vmo cam_vmo,
          zx::resource irq_resource)
      : PcirootBase(root_host),
        node_name_(std::move(node_name)),
        dispatcher_(dispatcher),
        iommu_(std::move(iommu)),
        cam_(std::move(cam_vmo)),
        irq_resource_(std::move(irq_resource)) {
    ZX_DEBUG_ASSERT(irq_resource_.is_valid());
    ZX_DEBUG_ASSERT(iommu_.is_valid());
  }
  virtual ~Pciroot() = default;
  zx::result<> CreateInterruptsAndRouting(
      std::span<const pci_dt::Gicv3InterruptMapElement> interrupts);

  // Implementation for fuchsia.hardware.pciroot provided via lib/pci. These
  // disambiguate between the `ddk::Protocol` static implementations and our
  // own.
  using PcirootBase::PcirootAllocateMsi;
  using PcirootBase::PcirootDriverShouldProxyConfig;
  using PcirootBase::PcirootGetAddressSpace;
  using PcirootBase::PcirootReadConfig16;
  using PcirootBase::PcirootReadConfig32;
  using PcirootBase::PcirootReadConfig8;
  using PcirootBase::PcirootWriteConfig16;
  using PcirootBase::PcirootWriteConfig32;
  using PcirootBase::PcirootWriteConfig8;

  // Methods that must be defiined per platform for fuchsia.hardware.pciroot
  zx_status_t PcirootGetBti(uint32_t bdf, uint32_t index, zx::bti* bti);
  zx_status_t PcirootGetPciPlatformInfo(pci_platform_info_t* info);

  pciroot_protocol_ops_t* pciroot_protocol_ops() { return &pciroot_protocol_ops_; }

 private:
  std::string node_name_;
  async_dispatcher_t* dispatcher_;
  fdf::ClientEnd<fuchsia_hardware_platform_bus::Iommu> iommu_;
  zx::vmo cam_;
  const zx::resource irq_resource_;
  std::vector<pci_legacy_irq_t> interrupts_;
  std::vector<pci_irq_routing_entry_t> irq_routing_entries_;
};

// Ideally Crosvm and Pciroot would be the same class but PciRootHost is not trivially
// constructable nor movable at this time which complicates overall construction.
class Crosvm : public fdf::DriverBase {
 public:
  Crosvm(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("crosvm", std::move(start_args), std::move(dispatcher)) {}
  ~Crosvm() = default;
  zx::result<> Start() override;
  zx::result<> CreateMetadata();
  // Create the `Pciroot` and any associated root host dependencies.
  zx::result<> CreatePciroot(const pci_dt::PciVisitor& pci_visitor);
  zx::result<> CreateRoothost(const pci_dt::PciVisitor& pci_visitor);
  // Bring up the compat server and serve the fuchsia.hardware.pciroot banjo service.
  zx::result<> StartBanjoServer();

 private:
  std::optional<PciRootHost> root_host_;
  std::optional<Pciroot> pciroot_;

  std::optional<compat::BanjoServer> banjo_server_;
  compat::SyncInitializedDeviceServer compat_server_;
  fdf_metadata::MetadataServer<fuchsia_hardware_pci::BoardConfiguration> metadata_server_;

  fidl::Client<fuchsia_driver_framework::NodeController> controller_;

  zx::resource io_resource_;
  zx::resource mmio_resource_;
  zx::resource msi_resource_;
};

}  // namespace board_crosvm

#endif  // SRC_DEVICES_BOARD_DRIVERS_CROSVM_CROSVM_H_

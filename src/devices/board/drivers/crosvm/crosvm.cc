// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "crosvm.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.kernel/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/zx/resource.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <bind/fuchsia/pci/cpp/bind.h>

#include "fidl/fuchsia.hardware.pci/cpp/natural_types.h"
#include "fuchsia/hardware/pciroot/c/banjo.h"
#include "lib/pci/devicetree.h"

namespace board_crosvm {

namespace {
const std::string kPcirootNodeName = "PCI0";
// all values here are taken directly from dts/crosvm.dts.
constexpr pci::RegPropertyElement kCrosvmReg{
    .phys_hi = 0x00, .phys_lo = 0x10000, .size_hi = 0x00, .size_lo = 0x1000000};
constexpr std::array kCrosvmRanges{
    pci::RangePropertyElement{.phys_hi = 0x3000000,
                              .phys_mid = 0x00,
                              .phys_lo = 0x2000000,
                              .parent_hi = 0x00,
                              .parent_lo = 0x2000000,
                              .size_hi = 0x00,
                              .size_lo = 0x2000000},
    pci::RangePropertyElement{.phys_hi = 0x3000000,
                              .phys_mid = 0x00,
                              .phys_lo = 0x90800000,
                              .parent_hi = 0x00,
                              .parent_lo = 0x90800000,
                              .size_hi = 0xff,
                              .size_lo = 0x6f800000},
};

// DFv2 does not expose get_mmio_resource() and the other methods for acquiring higher privilege
// resources so we need to obtain them ourselves.
template <class ResourceMoniker>
zx::result<zx::resource> GetResource(const std::shared_ptr<fdf::Namespace>& incoming) {
  zx::result result = incoming->Connect<ResourceMoniker>();
  if (result.is_error()) {
    return result.take_error();
  }
  fidl::WireResult wire_result = fidl::WireCall(result.value())->Get();
  if (!wire_result.ok()) {
    return zx::error(wire_result.status());
  }
  return zx::ok(std::move(wire_result.value().resource));
}
}  // namespace

zx::result<> Crosvm::CreateRoothost() {
  // Root host resource and construction is handled first.
  zx::result<zx::resource> msi{};
  if (msi = GetResource<fuchsia_kernel::MsiResource>(incoming()); msi.is_error()) {
    FDF_LOG(ERROR, "Couldn't obtain MSI resource: %s", msi.status_string());
    return msi.take_error();
  }
  msi_resource_ = *std::move(msi);

  // We need the MMIO resource to allocate the ECAM, as well as allowing the
  // root host to allocate exclusive MMIO regions for PCI BAR allocations.
  zx::result<zx::resource> mmio{};
  if (mmio = GetResource<fuchsia_kernel::MmioResource>(incoming()); mmio.is_error()) {
    FDF_LOG(ERROR, "Couldn't obtain MMIO resource: %s", mmio.status_string());
    return mmio.take_error();
  }
  mmio_resource_ = *std::move(mmio);

  // io_resource by design should not be used within Crosvm due to PCIe
  // standards with devicetree only using MMIO space.
  root_host_.emplace(msi_resource_.borrow(), mmio_resource_.borrow(), io_resource_.borrow(),
                     PCI_ADDRESS_SPACE_MEMORY);

  for (auto& range : kCrosvmRanges) {
    FDF_LOG(DEBUG, "%02X.%02X.%01X: %s base %#lx size %#zx %sprefetchable, %saliased",
            range.bus_number(), range.device_number(), range.function_number(),
            pci::AddressSpaceLabel(range.address_space()), range.child_address(), range.size(),
            (range.prefetchable()) ? "" : "non-", (range.aliased_or_below()) ? "" : "not ");
    ZX_DEBUG_ASSERT_MSG(range.address_space() == pci::AddressSpace::Mmio64,
                        "The ranges expected in kCrosvmRanges should only be 64 bit.");
    if (zx::result result = root_host_->AddMmioRange(range.child_address(), range.size());
        result.is_error()) {
      FDF_LOG(ERROR, "failed to add region [%#lx, %#lx) to MMIO allocators: %s",
              range.child_address(), range.child_address() + range.size(), result.status_string());
    }
  }

  return zx::ok();
}

zx::result<> Crosvm::CreateMetadata() {
  fuchsia_hardware_pci::BoardConfiguration board_config{
      {fuchsia_hardware_pci::UseIntxWorkaroundType()}};
  if (zx::result result = metadata_server_.SetMetadata(board_config); result.is_error()) {
    return result.take_error();
  }

  if (zx::result result = metadata_server_.Serve(*outgoing(), dispatcher()); result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> Crosvm::CreatePciroot() {
  zx::result iommu = incoming()->Connect<fuchsia_hardware_platform_bus::Service::Iommu>();
  if (iommu.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to iommu: %s", iommu.status_string());
    return iommu.take_error();
  }

  zx::vmo ecam;
  zx_status_t status =
      zx::vmo::create_physical(/*resource=*/mmio_resource_, /*paddr=*/kCrosvmReg.base(),
                               /*size=*/kCrosvmReg.size(), /*result=*/&ecam);
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to create allocate ECAM for PCI: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  root_host_->mcfgs().push_back(
      {.address = kCrosvmReg.base(), .pci_segment = 0, .start_bus_number = 0, .end_bus_number = 0});

  zx::result<zx::resource> irq;
  if (irq = GetResource<fuchsia_kernel::IrqResource>(incoming()); irq.is_error()) {
    FDF_LOG(ERROR, "Couldn't obtain IRQ resource: %s", irq.status_string());
    return irq.take_error();
  }

  pciroot_.emplace(kPcirootNodeName, &*root_host_, dispatcher(), std::move(iommu.value()),
                   std::move(ecam), std::move(irq.value()));

  if (zx::result<> result = pciroot_->CreateInterruptsAndRouting(); result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

zx::result<> Crosvm::StartBanjoServer() {
  banjo_server_.emplace(bind_fuchsia_pci::BIND_PROTOCOL_ROOT, &*pciroot_,
                        pciroot_->pciroot_protocol_ops());
  compat::DeviceServer::BanjoConfig banjo_config{
      .default_proto_id = bind_fuchsia_pci::BIND_PROTOCOL_ROOT,
  };
  banjo_config.callbacks[bind_fuchsia_pci::BIND_PROTOCOL_ROOT] = banjo_server_->callback();

  // Spin up the compat server for serving fuchsia.hardware.pciroot.
  zx::result<> result =
      compat_server_.Initialize(incoming(), outgoing(), node_name(), kPcirootNodeName,
                                compat::ForwardMetadata::All(), std::move(banjo_config));
  if (result.is_error()) {
    return result.take_error();
  }

  std::vector offers{compat_server_.CreateOffers2()};
  offers.push_back(metadata_server_.MakeOffer());

  zx::result child = AddChild(kPcirootNodeName, {{banjo_server_->property()}}, offers);
  if (child.is_error()) {
    return child.take_error();
  }

  controller_.Bind(std::move(child.value()), dispatcher());
  return zx::ok();
}

zx::result<> Crosvm::Start() {
  if (zx::result<> result = CreateRoothost(); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = CreateMetadata(); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = CreatePciroot(); result.is_error()) {
    return result.take_error();
  }

  if (zx::result<> result = StartBanjoServer(); result.is_error()) {
    return result.take_error();
  }

  return zx::ok();
}

}  // namespace board_crosvm

FUCHSIA_DRIVER_EXPORT(board_crosvm::Crosvm);

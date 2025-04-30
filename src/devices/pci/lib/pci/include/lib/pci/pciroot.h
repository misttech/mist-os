// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_PCI_LIB_PCI_INCLUDE_LIB_PCI_PCIROOT_H_
#define SRC_DEVICES_PCI_LIB_PCI_INCLUDE_LIB_PCI_PCIROOT_H_

#include <mistos/hardware/pciroot/cpp/banjo.h>
// #include <lib/inspect/cpp/inspect.h>
#include <lib/pci/root_host.h>
// #include <lib/zx/msi.h>
#include <stdint.h>
#include <string.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

// This class is a mix-in to provide an interface to inspect for Pciroot.
class PcirootInspect {
 public:
  static constexpr size_t kMaxRegionStringSize =
      sizeof("[0x0000000000000000, 0x0000000000000000) 0x000000000000000");
  static constexpr size_t kMaxSizeStringSize = sizeof("");
  static constexpr char kBoardMmioName[] = "Board MMIO Regions";
  static constexpr char kBoardIoName[] = "Board IO Regions";
  static constexpr char kAllocatedMmioName[] = "Allocated MMIO Regions";
  static constexpr char kAllocatedIoName[] = "Allocated IO Regions";

  // inspect::Inspector& inspect() { return inspect_; }

 protected:
  void InitializeInspect(PciRootHost* host) {
    // board_mmio_ = inspect_.GetRoot().CreateChild(kBoardMmioName);
    // board_io_ = inspect_.GetRoot().CreateChild(kBoardIoName);
    // allocated_mmio_ = inspect_.GetRoot().CreateChild(kAllocatedMmioName);
    // allocated_io_ = inspect_.GetRoot().CreateChild(kAllocatedIoName);

    // Add the regions to Pciroot from the Board Driver / RootHost. When
    // properly supporting multiple pciroots with an external driver this will
    // need to be moved into the Root host side of the driver.
    AddBoardRegionsToInspect(host);
  }

  void AddBoardRegionsToInspect(PciRootHost* host) {
    auto mmio_walk_fn = [this](const ralloc_region_t* region) -> bool {
      AddBoardMmioRegion(region);
      return true;
    };
    host->Mmio32().WalkAvailableRegions(mmio_walk_fn);
    host->Mmio64().WalkAvailableRegions(mmio_walk_fn);
    auto io_walk_fn = [this](const ralloc_region_t* region) -> bool {
      AddBoardIoRegion(region);
      return true;
    };
    host->Io().WalkAvailableRegions(io_walk_fn);
  }

  void AddAllocatedIoRegion(const ralloc_region_t region) {
    // static size_t index = 0;
    // AddRegionToInspect(allocated_io_, index++, &region);
  }

  void AddAllocatedMmioRegion(const ralloc_region_t region) {
    // static size_t index = 0;
    // AddRegionToInspect(allocated_mmio_, index++, &region);
  }

  void AddBoardIoRegion(const ralloc_region_t* region) {
    // static size_t index = 0;
    // AddRegionToInspect(board_io_, index++, region);
  }

  void AddBoardMmioRegion(const ralloc_region_t* region) {
    // static size_t index = 0;
    // AddRegionToInspect(board_mmio_, index++, region);
  }

  // This does the brunt of the work to give us inspect data that looks similar to:
  //  root:
  //  Allocated IO Regions:
  //    [0x700, 0x740) = 0x40
  //  ..
  //  Allocated MMIO Regions:
  //    [0xfd000000, 0xfe000000) = 0x1000000
  //  ..
  //  Board MMIO Regions:
  //    [0, 0x60) = 0x60
  //  ..
  //  Board MMIO Regions:
  //    [0x280000000, 0xa80000000) = 0x800000000
  static void AddRegionToInspect(/*inspect::Node& parent,*/ size_t index,
                                 const ralloc_region_t* region) {
    std::array<char, kMaxRegionStringSize> value;
    std::array<char, 8> key;
    snprintf(key.data(), key.size(), "%02zx", index);
    snprintf(value.data(), value.size(), "[%#lx, %#lx) %#lx", region->base,
             region->base + region->size, region->size);
    // parent.RecordString(key.data(), value.data());
  }

 private:
  // inspect::Inspector inspect_;
  // inspect::Node board_mmio_;
  // inspect::Node board_io_;
  // inspect::Node allocated_mmio_;
  // inspect::Node allocated_io_;
};
// PcirootBase is the interface between a platform's PCI RootHost, and the PCI Bus Driver instances.
// It is templated on |PlatformContextType| so that platform specific context can be provided to
// each root as necessary. For instance, in ACPI systems this contains the ACPI object for the PCI
// root to work with ACPICA.
//
// Many methods may overlap between platforms, but the metadata that a given platform may need to
// track can vary. To support this a PcirootBase class templated off the context type is provided
// here and platforms are expected to derive it and override the methods they need to implement.
class PcirootBase : public PcirootInspect {
 public:
  explicit PcirootBase(PciRootHost* host) : root_host_(host) { InitializeInspect(root_host_); }
  virtual ~PcirootBase() = default;

  // If |true| is returned by the Pciroot implementation then the bus driver
  // will send all config space reads and writes over the Pciroot protocol
  // rather than in the bus driver using MMIO/IO access. This exists to work
  // with non-standard PCI implementations that require controller configuration
  // before accessing a given device.
  bool PcirootDriverShouldProxyConfig() {
    // By default, if a platform has MMIO based ECAMs (MMCFG) then we assume it
    // is safe to have config handled in the bus driver through MMIO. This can
    // be overriden by a given derived Pciroot implementation for a specific
    // board target.
    return !root_host_->mcfgs().is_empty();
  }

  // Config space read/write accessors for PCI systems that require platform
  // bus to configure something before config space is accessible. For ACPI
  // systems we only intend to use PIO access if MMIO config is unavailable.
  // In the event we do use them though, we're restricted to the base 256 byte
  // PCI config header.
  virtual zx_status_t PcirootReadConfig8(const pci_bdf_t* address, uint16_t offset,
                                         uint8_t* value) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  virtual zx_status_t PcirootReadConfig16(const pci_bdf_t* address, uint16_t offset,
                                          uint16_t* value) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  virtual zx_status_t PcirootReadConfig32(const pci_bdf_t* address, uint16_t offset,
                                          uint32_t* value) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  virtual zx_status_t PcirootWriteConfig8(const pci_bdf_t* address, uint16_t offset,
                                          uint8_t value) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  virtual zx_status_t PcirootWriteConfig16(const pci_bdf_t* address, uint16_t offset,
                                           uint16_t value) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  virtual zx_status_t PcirootWriteConfig32(const pci_bdf_t* address, uint16_t offset,
                                           uint32_t value) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t PcirootAllocateMsi(uint32_t msi_count, bool can_target_64bit,
                                 uint8_t* out_allocation_list, size_t allocation_count,
                                 size_t* out_allocation_actual) {
    ZX_ASSERT(allocation_count == 1);

    // AllocateMsi already uses platform specific MSI impleemnation methods and
    // syscalls, so this likely suits most platforms.
    fbl::RefPtr<MsiDispatcher> msi;
    zx_status_t status = root_host_->AllocateMsi(msi_count, &msi);
    if (status != ZX_OK) {
      return status;
    }

    out_allocation_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&msi));
    ZX_ASSERT(out_allocation_list != nullptr);
    *out_allocation_actual = 1;
    return ZX_OK;
  }

  // Allocate out of the IO / MMIO32 allocators if required, otherwise try to use whichever
  // MMIO allocator can fulfill the given request of specified base and size.
  zx_status_t PcirootGetAddressSpace(uint64_t in_base, uint64_t size, pci_address_space_t type,
                                     bool low, uint64_t* out_base, uint8_t* out_resource_list,
                                     size_t resource_count, size_t* out_resource_actual,
                                     uint8_t* out_token_list, size_t token_count,
                                     size_t* out_token_actual) {
    ZX_ASSERT(resource_count == 1);
    ZX_ASSERT(token_count == 1);
    fbl::RefPtr<ResourceDispatcher> out_resource;
    fbl::RefPtr<EventPairDispatcher> out_eventpair;
    if (type == PCI_ADDRESS_SPACE_IO) {
      auto result = root_host_->AllocateIoWindow(in_base, size, &out_resource, &out_eventpair);
      if (result.is_ok()) {
        out_resource_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&out_resource));
        ZX_ASSERT(out_resource_list != nullptr);
        out_token_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&out_eventpair));
        ZX_ASSERT(out_token_list != nullptr);
        AddAllocatedIoRegion(ralloc_region_t{.base = result.value(), .size = size});
        *out_base = result.value();
      }
      return result.status_value();
    }

    if (!low) {
      auto result = root_host_->AllocateMmio64Window(in_base, size, &out_resource, &out_eventpair);
      if (result.is_ok()) {
        out_resource_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&out_resource));
        ZX_ASSERT(out_resource_list != nullptr);
        out_token_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&out_eventpair));
        ZX_ASSERT(out_token_list != nullptr);
        AddAllocatedMmioRegion(ralloc_region_t{.base = result.value(), .size = size});
        *out_base = result.value();
      }
      return result.status_value();
    }

    auto result = root_host_->AllocateMmio32Window(in_base, size, &out_resource, &out_eventpair);
    if (result.is_ok()) {
      out_resource_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&out_resource));
      ZX_ASSERT(out_resource_list != nullptr);
      out_token_list = reinterpret_cast<uint8_t*>(fbl::ExportToRawPtr(&out_eventpair));
      ZX_ASSERT(out_token_list != nullptr);
      AddAllocatedMmioRegion(ralloc_region_t{.base = result.value(), .size = size});
      *out_base = result.value();
    }
    return result.status_value();
  }

 private:
  // TODO(https://fxbug.dev/42108122): presently, pciroot instances will always outlive the root
  // host it references here because it exists within the same devhost process as a singleton. This
  // will be updated when the pciroot implementation changes to move away from a standalone banjo
  // protocol.
  PciRootHost* root_host_;
};

#endif  // SRC_DEVICES_PCI_LIB_PCI_INCLUDE_LIB_PCI_PCIROOT_H_

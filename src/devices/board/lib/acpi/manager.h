// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_LIB_ACPI_MANAGER_H_
#define SRC_DEVICES_BOARD_LIB_ACPI_MANAGER_H_

#include <lib/mistos/util/btree_map.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <kernel/mutex.h>

#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/bus-type.h"
#include "src/devices/board/lib/acpi/device-builder.h"
#include "src/devices/board/lib/acpi/power-resource.h"

namespace async {
class Executor;
}

namespace acpi_host_test {
class AcpiHostTest;
}

namespace acpi {

// Class that manages ACPI device discovery and publishing.
class Manager {
 public:
  // Construct a new manager.
  // |acpi| should be a pointer to the ACPI implementation to use. The caller keeps ownership and
  // must ensure it outlives the manager.
  // |iommu| should be a pointer to the IOMMU manager implementation. The caller keeps ownership and
  // must ensure it outlives the manager.
  // |acpi_root| is a pointer to the device that will be the parent of all other ACPI devices. It
  // should be owned by the DDK, and must outlive the manager.
  explicit Manager(Acpi* acpi) : acpi_(acpi) {}

  virtual ~Manager() = default;

  // Walk the ACPI tree, keeping track of each device that's found.
  acpi::status<> DiscoverDevices();
  // Infer information about devices based on their relationships.
  acpi::status<> ConfigureDiscoveredDevices();
  // Publish devices to driver manager.
  acpi::status<> PublishDevices();

  // For devices: get the next unique BTI ID.
  //uint32_t GetNextBtiId() { return next_bti_++; }

  // Used by a device to inform the manager about the existence of a power resource.
  const PowerResource* AddPowerResource(ACPI_HANDLE power_resource_handle);

  // Used by a device to declare that it requires a list of power resources to be on.
  // The list of power resources should be sorted by ascending resource_order.
  zx_status_t ReferencePowerResources(const fbl::Vector<ACPI_HANDLE>& power_resource_handles);

  // Used by a device to declare that it no longer requires a list of power resources to be on.
  // The list of power resources should be sorted by ascending resource_order.
  zx_status_t DereferencePowerResources(const fbl::Vector<ACPI_HANDLE>& power_resource_handles);

  // For internal and unit test use only.
  DeviceBuilder* LookupDevice(ACPI_HANDLE handle);

  Acpi* acpi() { return acpi_; }

 private:
  friend acpi_host_test::AcpiHostTest;
  // Returns true if the device is not present, and it and its children should be ignored.
  // Returns false if the device is present and its children can be enumerated.
  acpi::status<bool> DiscoverDevice(ACPI_HANDLE handle);

  // Call pci_init for the given device.
  acpi::status<> PublishPciBus(DeviceBuilder* device);

  Acpi* acpi_;
  util::BTreeMap<ACPI_HANDLE, DeviceBuilder> devices_;

  DECLARE_MUTEX(Manager) devices_lock_;
  DECLARE_MUTEX(Manager) power_resources_lock_;
  util::BTreeMap<ACPI_HANDLE, PowerResource> power_resources_ __TA_GUARDED(power_resources_lock_);

  fbl::Vector<ACPI_HANDLE> device_publish_order_;
  util::BTreeMap<BusType, uint32_t> next_bus_ids_;
  bool published_pci_bus_ = false;
  uint32_t device_id_ = 1;
  uint32_t next_bti_ = 0;
};

}  // namespace acpi

#endif  // SRC_DEVICES_BOARD_LIB_ACPI_MANAGER_H_

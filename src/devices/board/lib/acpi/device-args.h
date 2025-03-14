// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_LIB_ACPI_DEVICE_ARGS_H_
#define SRC_DEVICES_BOARD_LIB_ACPI_DEVICE_ARGS_H_

#include <vector>

#include <acpica/acpi.h>

#include "src/devices/board/lib/acpi/manager.h"

using pci_bdf_t = struct pci_bdf {
  uint8_t bus_id;
  uint8_t device_id;
  uint8_t function_id;
};

namespace acpi {

inline const char* BusTypeToString(BusType t) {
  switch (t) {
    case kPci:
      return "pci";
    case kSpi:
      return "spi";
    case kI2c:
      return "i2c";
    case kUnknown:
      return "unknown";
  }
}

struct DeviceArgs {
  acpi::Manager* manager_;
  ACPI_HANDLE handle_;

  // Bus metadata
  BusMetadata metadata_;
  BusType bus_type_ = BusType::kUnknown;
  uint32_t bus_id_ = UINT32_MAX;

  // PCI metadata
  fbl::Vector<pci_bdf_t> bdfs_;

  DeviceArgs(acpi::Manager* manager, ACPI_HANDLE handle) : manager_(manager), handle_(handle) {}
  DeviceArgs(DeviceArgs&) = delete;

  DeviceArgs& SetBusMetadata(BusMetadata metadata, BusType bus_type, uint32_t bus_id) {
    metadata_ = std::move(metadata);
    bus_type_ = bus_type;
    bus_id_ = bus_id;
    return *this;
  }
  DeviceArgs& SetPciMetadata(fbl::Vector<pci_bdf_t> bdfs) {
    bdfs_ = std::move(bdfs);
    return *this;
  }
};

}  // namespace acpi

#endif  // SRC_DEVICES_BOARD_LIB_ACPI_DEVICE_ARGS_H_

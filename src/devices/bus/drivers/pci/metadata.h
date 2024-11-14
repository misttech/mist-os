// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BUS_DRIVERS_PCI_METADATA_H_
#define SRC_DEVICES_BUS_DRIVERS_PCI_METADATA_H_

#include <fidl/fuchsia.hardware.pci/cpp/fidl.h>

#ifdef SRC_LIB_DDKTL_INCLUDE_DDKTL_DEVICE_H_
#include <ddktl/metadata_server.h>
namespace ddk {
#else
#include <sdk/lib/driver/metadata/cpp/metadata_server.h>
namespace fdf_metadata {
#endif

template <>
struct ObjectDetails<fuchsia_hardware_pci::BoardConfiguration> {
  inline static const char* Name = fuchsia_hardware_pci::kMetadataTypeName;
};

}  // namespace ddk (or fdf_metadata)

#endif  // SRC_DEVICES_BUS_DRIVERS_PCI_METADATA_H_

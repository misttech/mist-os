// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.sdmmc/cpp/driver/fidl.h>

#include <ddktl/metadata_server.h>

#ifndef SRC_DEVICES_BLOCK_LIB_SDMMC_METADATA_METADATA_LEGACY_H_
#define SRC_DEVICES_BLOCK_LIB_SDMMC_METADATA_METADATA_LEGACY_H_

namespace ddk {

template <>
struct ObjectDetails<fuchsia_hardware_sdmmc::SdmmcMetadata> {
  inline static const char* Name = fuchsia_hardware_sdmmc::kMetadataTypeName;
};

template <>
struct ObjectDetails<fuchsia_hardware_sdmmc::wire::SdmmcMetadata> {
  inline static const char* Name = fuchsia_hardware_sdmmc::kMetadataTypeName;
};

}  // namespace ddk

namespace sdmmc {

using MetadataServer = ddk::MetadataServer<fuchsia_hardware_sdmmc::SdmmcMetadata>;

}  // namespace sdmmc

#endif  // SRC_DEVICES_BLOCK_LIB_SDMMC_METADATA_METADATA_LEGACY_H_

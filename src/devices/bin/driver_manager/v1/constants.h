// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_CONSTANTS_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_CONSTANTS_H_

#include <string_view>

#include "src/devices/bin/driver_manager/v1/driver.h"

namespace fdf {

constexpr std::string_view kFragmentDriverUrl = "#meta/fragment.cm";
constexpr std::string_view kFragmentProxyDriverUrl = "#meta/fragment.proxy.cm";

const MatchedDriverInfo kFragmentDriverInfo = {
    .colocate = true,
    .is_dfv2 = false,
    .package_type = fuchsia_driver_index::DriverPackageType::kBoot,
    .component_url = std::string(kFragmentDriverUrl),
};

const MatchedDriverInfo kFragmentProxyDriverInfo = {
    .colocate = true,
    .is_dfv2 = false,
    .package_type = fuchsia_driver_index::DriverPackageType::kBoot,
    .component_url = std::string(kFragmentProxyDriverUrl),
};

}  // namespace fdf

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_CONSTANTS_H_

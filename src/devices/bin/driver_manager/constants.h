// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_CONSTANTS_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_CONSTANTS_H_

#include <string_view>

namespace fdf {

constexpr std::string_view kFragmentDriverUrl = "#driver/fragment.so";
constexpr std::string_view kFragmentProxyDriverUrl = "#driver/fragment.proxy.so";

}  // namespace fdf

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_CONSTANTS_H_

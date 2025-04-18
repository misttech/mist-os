// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_SYMBOLS_H_
#define VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_SYMBOLS_H_

namespace mistos {

struct device_t {
  const char* name;
  void* context;
};

constexpr device_t kDefaultDevice = {
    .name = "mistos-device",
    .context = nullptr,
};

// The symbol for the compat device: device_t.
constexpr char kDeviceSymbol[] = "mistos.device/Device";

}  // namespace mistos

#endif  // VENDOR_MISTTECH_SRC_DEVICES_MISC_DRIVERS_MISTOS_INCLUDE_MISC_DRIVERS_MISTOS_SYMBOLS_H_

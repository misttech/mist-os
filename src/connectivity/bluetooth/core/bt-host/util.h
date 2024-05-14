// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_UTIL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_UTIL_H_

#include <fidl/fuchsia.hardware.bluetooth/cpp/fidl.h>
#include <lib/async/dispatcher.h>

#include <string>

#include <pw_async_fuchsia/dispatcher.h>

namespace bthost {

// Creates a FIDL channel connecting to the service directory at |device_path| relative to
// component's namespace. Creates and returns a VendorHandle using the client end of the channel if
// successful, otherwise returns nullptr on failure.
zx::result<fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor>> CreateVendorHandle(
    const std::string& device_path);

}  // namespace bthost

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_UTIL_H_

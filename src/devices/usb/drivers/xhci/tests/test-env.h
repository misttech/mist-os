// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_XHCI_TESTS_TEST_ENV_H_
#define SRC_DEVICES_USB_DRIVERS_XHCI_TESTS_TEST_ENV_H_

#include <lib/driver/testing/cpp/driver_test.h>

#include <gtest/gtest.h>

#include "src/devices/usb/drivers/xhci/usb-xhci.h"
#include "src/lib/testing/predicates/status.h"

namespace usb_xhci {

const zx::bti kFakeBti(42);

class EmptyTestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override { return zx::ok(); }
};

class EmptyTestConfig final {
 public:
  using DriverType = usb_xhci::UsbXhci;
  using EnvironmentType = EmptyTestEnvironment;
};

}  // namespace usb_xhci

#endif  // SRC_DEVICES_USB_DRIVERS_XHCI_TESTS_TEST_ENV_H_

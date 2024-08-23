// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_USB_DRIVERS_XHCI_TESTS_TEST_ENV_H_
#define SRC_DEVICES_USB_DRIVERS_XHCI_TESTS_TEST_ENV_H_

#include <lib/zx/bti.h>

namespace usb_xhci {

const zx::bti kFakeBti(42);

}  // namespace usb_xhci

#endif  // SRC_DEVICES_USB_DRIVERS_XHCI_TESTS_TEST_ENV_H_

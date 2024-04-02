// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_USB_PHY_H_
#define SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_USB_PHY_H_

#include <zircon/types.h>

enum UsbProtocol : uint8_t {
  Usb2_0 = 2,
  Usb3_0 = 3,
};

enum UsbMode : uint8_t {
  Unknown = 0,
  Host = 1,
  Peripheral = 2,
  Otg = 3,
};

struct UsbPhyMode {
  UsbProtocol protocol;
  UsbMode dr_mode;
  bool is_otg_capable;
};

static const uint32_t DEVICE_METADATA_PRIVATE_PHY_TYPE = 0x59485000;  // 'PHY\0'
enum PhyType : uint8_t {
  kG12A = 0,
  kG12B = 1,
};

#endif  // SRC_DEVICES_LIB_AMLOGIC_INCLUDE_SOC_AML_COMMON_AML_USB_PHY_H_

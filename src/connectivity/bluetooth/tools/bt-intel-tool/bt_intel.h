// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_TOOLS_BT_INTEL_TOOL_BT_INTEL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_TOOLS_BT_INTEL_TOOL_BT_INTEL_H_

#include <fbl/macros.h>

#include "src/connectivity/bluetooth/core/bt-host/common/device_address.h"
#include "src/connectivity/bluetooth/core/bt-host/hci-spec/protocol.h"

namespace bt_intel {

constexpr ::bt::hci_spec::OpCode kLoadPatch = ::bt::hci_spec::VendorOpCode(0x008e);

constexpr ::bt::hci_spec::OpCode kReadVersion = ::bt::hci_spec::VendorOpCode(0x0005);

struct IntelVersionReturnParams {
  ::pw::bluetooth::emboss::StatusCode status;
  uint8_t hw_platform;
  uint8_t hw_variant;
  uint8_t hw_revision;
  uint8_t fw_variant;
  uint8_t fw_revision;
  uint8_t fw_build_num;
  uint8_t fw_build_week;
  uint8_t fw_build_year;
  uint8_t fw_patch_num;
} __PACKED;

constexpr bt::hci_spec::OpCode kSecureSend = ::bt::hci_spec::VendorOpCode(0x0009);
constexpr ::bt::hci_spec::OpCode kReadBootParams = ::bt::hci_spec::VendorOpCode(0x000D);

struct IntelReadBootParamsReturnParams {
  ::pw::bluetooth::emboss::StatusCode status;
  uint8_t otp_format;
  uint8_t otp_content;
  uint8_t otp_patch;
  uint16_t dev_revid;
  ::pw::bluetooth::emboss::GenericEnableParam secure_boot;
  uint8_t key_from_hdr;
  uint8_t key_type;
  ::pw::bluetooth::emboss::GenericEnableParam otp_lock;
  ::pw::bluetooth::emboss::GenericEnableParam api_lock;
  ::pw::bluetooth::emboss::GenericEnableParam debug_lock;
  ::bt::DeviceAddressBytes otp_bdaddr;
  uint8_t min_fw_build_num;
  uint8_t min_fw_build_week;
  uint8_t min_fw_build_year;
  ::pw::bluetooth::emboss::GenericEnableParam limited_cce;
  uint8_t unlocked_state;
} __PACKED;

constexpr ::bt::hci_spec::OpCode kReset = ::bt::hci_spec::VendorOpCode(0x0001);

struct IntelResetCommandParams {
  uint8_t data[8];
} __PACKED;

constexpr bt::hci_spec::OpCode kMfgModeChange = bt::hci_spec::VendorOpCode(0x0011);

enum class MfgDisableMode : uint8_t {
  kNoPatches = 0x00,
  kPatchesDisabled = 0x01,
  kPatchesEnabled = 0x02,
};

struct IntelMfgModeChangeCommandParams {
  ::pw::bluetooth::emboss::GenericEnableParam enable;
  MfgDisableMode disable_mode;
} __PACKED;

struct IntelSecureSendEventParams {
  uint8_t vendor_event_code;
  uint8_t result;
  uint16_t opcode;
  uint8_t status;
} __PACKED;

struct IntelBootloaderVendorEventParams {
  IntelBootloaderVendorEventParams() = delete;
  DISALLOW_COPY_ASSIGN_AND_MOVE(IntelBootloaderVendorEventParams);

  uint8_t vendor_event_code;
  uint8_t vendor_params[];
} __PACKED;

}  // namespace bt_intel

#endif  // SRC_CONNECTIVITY_BLUETOOTH_TOOLS_BT_INTEL_TOOL_BT_INTEL_H_

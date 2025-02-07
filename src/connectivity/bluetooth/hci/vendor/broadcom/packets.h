// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_PACKETS_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_PACKETS_H_

#include <endian.h>
#include <stdint.h>
#include <zircon/compiler.h>

#include <cstddef>
#include <limits>

namespace bt_hci_broadcom {

// Official HCI definitions:

struct HciCommandHeader {
  uint16_t opcode;
  uint8_t parameter_total_size;
} __PACKED;

constexpr uint16_t kResetCmdOpCode = 0x0c03;
constexpr uint16_t kReadBdaddrCmdOpCode = 0x1009;

struct HciEventHeader {
  uint8_t event_code;
  uint8_t parameter_total_size;
} __PACKED;

struct HciCommandComplete {
  HciEventHeader header;
  uint8_t num_hci_command_packets;
  uint16_t command_opcode;
  uint8_t return_code;
} __PACKED;

constexpr uint8_t kHciEvtCommandCompleteEventCode = 0x0e;

constexpr size_t kMacAddrLen = 6;

struct ReadBdaddrCommandComplete {
  HciCommandComplete header;
  uint8_t bdaddr[kMacAddrLen];
} __PACKED;

// Vendor HCI definitions:

constexpr uint16_t kStartFirmwareDownloadCmdOpCode = 0xfc2e;

struct BcmSetBaudRateCmd {
  HciCommandHeader header;
  uint16_t unused;
  uint32_t baud_rate;
} __PACKED;
constexpr uint16_t kBcmSetBaudRateCmdOpCode = 0xfc18;

struct BcmSetBdaddrCmd {
  HciCommandHeader header;
  uint8_t bdaddr[kMacAddrLen];
} __PACKED;
constexpr uint16_t kBcmSetBdaddrCmdOpCode = 0xfc01;

struct BcmSetAclPriorityCmd {
  HciCommandHeader header;
  uint16_t connection_handle;
  uint8_t priority;
  uint8_t direction;
} __PACKED;

constexpr uint16_t kBcmSetAclPriorityCmdOpCode = ((0x3F << 10) | 0x11A);
constexpr uint8_t kBcmAclPriorityNormal = 0x00;
constexpr uint8_t kBcmAclPriorityHigh = 0x01;
constexpr uint8_t kBcmAclDirectionSource = 0x00;
constexpr uint8_t kBcmAclDirectionSink = 0x01;
constexpr size_t kBcmSetAclPriorityCmdSize = sizeof(BcmSetAclPriorityCmd);

constexpr size_t kMaxHciCommandSize =
    sizeof(HciCommandHeader) +
    std::numeric_limits<decltype(HciCommandHeader::parameter_total_size)>::max();

// Max size of an event frame.
constexpr size_t kChanReadBufLen =
    sizeof(HciEventHeader) +
    std::numeric_limits<decltype(HciEventHeader::parameter_total_size)>::max();

// Min event param size for a valid command complete event frame
constexpr size_t kMinEvtParamSize = sizeof(HciCommandComplete) - sizeof(HciEventHeader);

// HCI reset command
const HciCommandHeader kResetCmd = {
    .opcode = htole16(kResetCmdOpCode),
    .parameter_total_size = 0,
};

// vendor command to begin firmware download
const HciCommandHeader kStartFirmwareDownloadCmd = {
    .opcode = htole16(kStartFirmwareDownloadCmdOpCode),
    .parameter_total_size = 0,
};

// HCI command to read BDADDR from controller
const HciCommandHeader kReadBdaddrCmd = {
    .opcode = htole16(kReadBdaddrCmdOpCode),
    .parameter_total_size = 0,
};

// Set Max TX Power

struct BcmSetPowerCapCmd {
  HciCommandHeader header;
  uint8_t sub_opcode;
  uint16_t cmd_format_opcode;
  uint8_t chain_0_power_limit_br;
  uint8_t chain_0_power_limit_edr;
  uint8_t chain_0_power_limit_ble;
  uint8_t chain_1_power_limit_br;
  uint8_t chain_1_power_limit_edr;
  uint8_t chain_1_power_limit_ble;
  uint8_t beamforming_cap[6];
} __PACKED;

constexpr uint16_t kBcmSetPowerCapCmdOpCode = 0xff00;
constexpr uint8_t kBcmSetPowerCapSubOpCode = 0x01;
constexpr uint16_t kBcmSetPowerCapCmdFormatOpCode = 0x0002;
constexpr uint8_t kBcmSetPowerCmdParamSize = sizeof(BcmSetPowerCapCmd) - sizeof(HciCommandHeader);

constexpr uint8_t kDefaultBrPowerCap = 72;
constexpr uint8_t kDefaultEdrPowerCap = 60;
constexpr uint8_t kDefaultBlePowerCap = 28;

constexpr BcmSetPowerCapCmd kDefaultPowerCapCmd = {
    .header = {.opcode = kBcmSetPowerCapCmdOpCode,
               .parameter_total_size = kBcmSetPowerCmdParamSize},
    .sub_opcode = kBcmSetPowerCapSubOpCode,
    .cmd_format_opcode = kBcmSetPowerCapCmdFormatOpCode,
    .chain_0_power_limit_br = kDefaultBrPowerCap,
    .chain_0_power_limit_edr = kDefaultEdrPowerCap,
    .chain_0_power_limit_ble = kDefaultBlePowerCap,
    .chain_1_power_limit_br = kDefaultBrPowerCap,
    .chain_1_power_limit_edr = kDefaultEdrPowerCap,
    .chain_1_power_limit_ble = kDefaultBlePowerCap,
    .beamforming_cap =
        {
            kDefaultBrPowerCap,
            kDefaultEdrPowerCap,
            kDefaultBlePowerCap,
            kDefaultBrPowerCap,
            kDefaultEdrPowerCap,
            kDefaultBlePowerCap,
        },
};

}  // namespace bt_hci_broadcom

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_PACKETS_H_

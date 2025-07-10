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

enum class BcmSleepMode : uint8_t {
  kDisabled = 0,
  kUart = 1,
  kReserved = 2,
  kUsb = 3,
};

// Write Sleep Mode Command
struct BcmWriteSleepModeCmd {
  HciCommandHeader header;
  BcmSleepMode mode;
  // Idle threshold for host, in 12.5ms units.
  uint8_t idle_threshold_host;
  // Idle threshold for device, in 12.5ms units.
  uint8_t idle_threshold_device;
  // Wake polarity for the device pin
  uint8_t bt_wake_polarity;
  // Wake polarity for the host pin
  uint8_t host_wake_polarity;
  // Allow host to sleep during SCO
  uint8_t sleep_during_sco;
  // Combine sleep mode and LPM
  uint8_t combine_sleep_and_lpm;
  // Whether to tri-state the UART before sleep
  uint8_t tri_state_uart_before_sleep;
  // USB flags (unused here)
  uint8_t usb_flags[3];
  // Pulsed HOST_WAKE
  uint8_t pulsed_host_wake;
} __PACKED;

constexpr uint16_t kBcmWriteSleepModeCmdOpCode = 0xfc27;
constexpr uint8_t kBcmWriteSleepModeParamSize =
    sizeof(BcmWriteSleepModeCmd) - sizeof(HciCommandHeader);

constexpr BcmWriteSleepModeCmd kDisableLowPowerModeCmd = {
    .header = {.opcode = kBcmWriteSleepModeCmdOpCode,
               .parameter_total_size = kBcmWriteSleepModeParamSize},
    .mode = BcmSleepMode::kDisabled,
    .idle_threshold_host = 0,
    .idle_threshold_device = 0,
    .bt_wake_polarity = 0,
    .host_wake_polarity = 0,
    .sleep_during_sco = 0,
    .combine_sleep_and_lpm = 0,
    .tri_state_uart_before_sleep = 0,
    .usb_flags = {0, 0, 0},
    .pulsed_host_wake = 0,
};

}  // namespace bt_hci_broadcom

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_BROADCOM_PACKETS_H_

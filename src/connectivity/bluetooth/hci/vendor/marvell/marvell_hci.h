// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_MARVELL_HCI_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_MARVELL_HCI_H_

#include <cstdint>

// Constants used in HCI frames

// Value in the ogf bits of the opcode (command or event) that indicates that the command is vendor-
// defined. This constant should be moved outside of the driver and into the generic bluetooth HCI
// constants.
constexpr uint16_t kHciOgfVendorSpecificDebug = 0x3f;

// Set Mac Address command (Opcode ocf bits)
constexpr uint16_t kHciOcfMarvellSetMacAddr = 0x22;

// Composed opcode for Marvell-specific SetMacAddr operation
constexpr uint16_t kHciOpcodeMarvellSetMacAddr =
    ((kHciOgfVendorSpecificDebug << 10) | kHciOcfMarvellSetMacAddr);

// Code in the HCI event header indicating that the event indicates command completion. This
// definition should also be moved outside of the driver and into the generic bluetooth HCI
// constants.
constexpr uint16_t kHciEventCodeCommandComplete = 0xe;

// Value in CommandCompleteEvent return_parameters for a successful Marvell SetMacAddr command
constexpr uint8_t kHciMarvellCmdCompleteSuccess = 0x0;

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_MARVELL_HCI_H_

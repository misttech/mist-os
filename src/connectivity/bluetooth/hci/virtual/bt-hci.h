// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_BT_HCI_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_BT_HCI_H_

#include <stdbool.h>
#include <stdint.h>

// Potential values for the flags bitfield in a snoop channel packet.
typedef uint32_t bt_hci_snoop_type_t;
#define BT_HCI_SNOOP_TYPE_CMD ((bt_hci_snoop_type_t)0)
#define BT_HCI_SNOOP_TYPE_EVT ((bt_hci_snoop_type_t)1)
#define BT_HCI_SNOOP_TYPE_ACL ((bt_hci_snoop_type_t)2)
#define BT_HCI_SNOOP_TYPE_SCO ((bt_hci_snoop_type_t)3)

#define BT_HCI_SNOOP_FLAG_RECV 0x04  // Host -> Controller

static inline uint8_t bt_hci_snoop_flags(bt_hci_snoop_type_t type, bool is_received) {
  return (uint8_t)(type | (is_received ? BT_HCI_SNOOP_FLAG_RECV : 0x00));
}

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VIRTUAL_BT_HCI_H_

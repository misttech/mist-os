// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_HOST_CHANNEL_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_HOST_CHANNEL_H_

#include <lib/zx/channel.h>

#include <string>

namespace bt_hci_marvell {

// These constants are used by the controller to identify the category of information being passed
// to or from it.
enum class ControllerChannelId {
  kChannelCommand = 0x1,  // host => controller
  kChannelAclData = 0x2,  // host <=> controller
  kChannelScoData = 0x3,  // host <=> controller
  kChannelEvent = 0x4,    // host <= controller
  kChannelVendor = 0xfe,  // host => controller
};

// Represents a channel used for communication with the host, and its associated attributes
class HostChannel {
 public:
  HostChannel(zx::channel channel, ControllerChannelId read_id, ControllerChannelId write_id,
              const char* name)
      : channel_(std::move(channel)), read_id_(read_id), write_id_(write_id), name_(name) {}

  // Simple accessors
  const zx::channel& channel() const { return channel_; }
  ControllerChannelId read_id() const { return read_id_; }
  ControllerChannelId write_id() const { return write_id_; }
  const char* name() const { return name_; }

 private:
  zx::channel channel_;

  // When read from the channel, what ID do we associate a packet with for sending it to the
  // controller?
  ControllerChannelId read_id_;

  // When we receive frames with this ID in the header, they should be written to this channel
  ControllerChannelId write_id_;

  // Human-readable name
  const char* name_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_HOST_CHANNEL_H_

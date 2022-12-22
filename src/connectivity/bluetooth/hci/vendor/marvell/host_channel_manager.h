// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_HOST_CHANNEL_MANAGER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_HOST_CHANNEL_MANAGER_H_

#include <functional>
#include <list>

#include "src/connectivity/bluetooth/hci/vendor/marvell/host_channel.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/interrupt_key_allocator.h"

namespace bt_hci_marvell {

// Track all of the open HostChannels. We use a highly-unsophisticated std::list container, but the
// number of open channels should be low single-digits. If this changes, it would be relatively
// easy to add efficient indices.
// Note: this class is not thread-safe.
class HostChannelManager {
 public:
  // Add a new channel and associated information. Failure conditions:
  // - A channel already exists with the specified write_id
  // - A channel already exists with the specified interrupt_key
  // On failure, nullptr is returned
  // On success, a pointer to the new HostChannel is returned
  const HostChannel* AddChannel(zx::channel channel, ControllerChannelId read_id,
                                ControllerChannelId write_id, uint64_t interrupt_key,
                                const char* name);

  // Remove the channel with the specified write_id, if such a channel exists.
  void RemoveChannel(ControllerChannelId write_id);

  // Find the channel with the specified write_id. Returns nullptr if no such channel exists.
  const HostChannel* HostChannelFromWriteId(ControllerChannelId write_id) const;

  // Find the channel with the specified interrupt key. Returns nullptr if no such channel exists.
  const HostChannel* HostChannelFromInterruptKey(uint64_t key) const;

  // AddChannel() and RemoveChannel() should not be called in by a |fn| passed to ForEveryChannel().
  void ForEveryChannel(std::function<void(const HostChannel*)> fn) const;

 private:
  std::list<HostChannel> open_channels_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_HOST_CHANNEL_MANAGER_H_

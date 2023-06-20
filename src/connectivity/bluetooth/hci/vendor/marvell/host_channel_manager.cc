// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "host_channel_manager.h"

#include <inttypes.h>
#include <lib/ddk/debug.h>
#include <zircon/assert.h>

namespace bt_hci_marvell {

const HostChannel* HostChannelManager::AddChannel(zx::channel channel, ControllerChannelId read_id,
                                                  ControllerChannelId write_id,
                                                  uint64_t interrupt_key, const char* name) {
  // We don't support a forwarding a single packet to multiple channels, so if we already have a
  // channel open with this write id, we can't create another
  const HostChannel* existing_host_channel = HostChannelFromWriteId(write_id);
  if (existing_host_channel != nullptr) {
    zxlogf(ERROR, "Failed to allocate HostChannel - id %d already allocated to %s channel",
           static_cast<int>(write_id), existing_host_channel->name());
    return nullptr;
  }

  // Interrupt keys should also be unique across all open channels.
  existing_host_channel = HostChannelFromInterruptKey(interrupt_key);
  if (existing_host_channel != nullptr) {
    zxlogf(ERROR,
           "Failed to allocate HostChannel - key %" PRIu64 "is already allocated to %s channel",
           interrupt_key, existing_host_channel->name());
    return nullptr;
  }

  auto iter = open_channels_.emplace(open_channels_.end(), std::move(channel), read_id, write_id,
                                     interrupt_key, name);
  return &(*iter);
}

// Deleting the channel will close it.
void HostChannelManager::RemoveChannel(ControllerChannelId write_id) {
  open_channels_.remove_if(
      [write_id](const HostChannel& channel) { return channel.write_id() == write_id; });
}

const HostChannel* HostChannelManager::HostChannelFromWriteId(ControllerChannelId write_id) const {
  for (const HostChannel& host_channel : open_channels_) {
    if (host_channel.write_id() == write_id) {
      return &host_channel;
    }
  }
  return nullptr;
}

const HostChannel* HostChannelManager::HostChannelFromInterruptKey(uint64_t key) const {
  for (const HostChannel& host_channel : open_channels_) {
    if (host_channel.interrupt_key() == key) {
      return &host_channel;
    }
  }
  return nullptr;
}

void HostChannelManager::ForEveryChannel(std::function<void(const HostChannel*)> fn) const {
  for (const HostChannel& host_channel : open_channels_) {
    fn(&host_channel);
  }
}

}  // namespace bt_hci_marvell

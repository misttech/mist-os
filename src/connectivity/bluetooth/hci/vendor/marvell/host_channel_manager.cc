// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "host_channel_manager.h"

#include <lib/ddk/debug.h>
#include <zircon/assert.h>

namespace bt_hci_marvell {

const HostChannel* HostChannelManager::AddChannel(zx::channel channel, ControllerChannelId read_id,
                                                  ControllerChannelId write_id, const char* name) {
  // We don't support a forwarding a single packet to multiple channels, so if we already have a
  // channel open with this write id, we can't create another
  if (HostChannelFromWriteId(write_id)) {
    zxlogf(ERROR, "Failed to allocate HostChannel - id %d already allocated", write_id);
    return nullptr;
  }

  auto iter =
      open_channels_.emplace(open_channels_.end(), std::move(channel), read_id, write_id, name);
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

// Apply a function to every channel
void HostChannelManager::ForEveryChannel(std::function<void(const HostChannel*)> fn) const {
  for (const HostChannel& host_channel : open_channels_) {
    fn(&host_channel);
  }
}

}  // namespace bt_hci_marvell

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_MANAGER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_MANAGER_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/weak_self.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/command_channel.h"

namespace bt::iso {

// Responsible for owning and managing IsoStream objects associated with a
// single LE connection.
// When operating as a Central, establishes an outgoing streams. When operating
// as a Peripheral, processes incoming stream requests .
class IsoStreamManager final {
 public:
  explicit IsoStreamManager(hci::CommandChannel::WeakPtr cmd_channel);
  ~IsoStreamManager();

  using WeakPtr = WeakSelf<IsoStreamManager>::WeakPtr;
  IsoStreamManager::WeakPtr GetWeakPtr() { return weak_self_.GetWeakPtr(); }

 private:
  hci::CommandChannel::WeakPtr cmd_;
  WeakSelf<IsoStreamManager> weak_self_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(IsoStreamManager);
};

}  // namespace bt::iso

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_MANAGER_H_

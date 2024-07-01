// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_H_

#include <cstdint>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/weak_self.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/hci-spec/protocol.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_common.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/command_channel.h"

namespace bt::iso {

class IsoStream final {
 public:
  IsoStream(uint8_t cig_id,
            uint8_t cis_id,
            hci_spec::ConnectionHandle cis_handle,
            CisEstablishedCallback cb)
      : cig_id_(cig_id),
        cis_id_(cis_id),
        cis_hci_handle_(cis_handle),
        cis_established_cb_(std::move(cb)),
        weak_self_(this) {}

  using WeakPtr = WeakSelf<IsoStream>::WeakPtr;
  WeakPtr GetWeakPtr() { return weak_self_.GetWeakPtr(); }

 private:
  uint8_t cig_id_ __attribute__((unused));
  uint8_t cis_id_ __attribute__((unused));

  // Handle assigned by the controller
  hci_spec::ConnectionHandle cis_hci_handle_ __attribute__((unused));

  CisEstablishedCallback cis_established_cb_ __attribute__((unused));

  WeakSelf<IsoStream> weak_self_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(IsoStream);
};

}  // namespace bt::iso

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_H_

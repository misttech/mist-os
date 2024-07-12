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
            CisEstablishedCallback cb,
            hci::CommandChannel::WeakPtr cmd_channel,
            pw::Callback<void()> on_closed_cb);

  // Handler for incoming HCI_LE_CIS_Established events. Returns a value
  // indicating whether the vent was handled.
  bool OnCisEstablished(const hci::EmbossEventPacket& event);

  // Terminate this stream.
  void Close();

  using WeakPtr = WeakSelf<IsoStream>::WeakPtr;
  WeakPtr GetWeakPtr() { return weak_self_.GetWeakPtr(); }

 private:
  enum class IsoStreamState {
    kNotEstablished,
    kEstablished,
  } state_;

  uint8_t cig_id_ __attribute__((unused));
  uint8_t cis_id_ __attribute__((unused));

  // Connection parameters, only valid after CIS is established
  CisEstablishedParameters cis_params_;

  // Handle assigned by the controller
  hci_spec::ConnectionHandle cis_hci_handle_;

  // Called after HCI_LE_CIS_Established event is received and handled
  CisEstablishedCallback cis_established_cb_;

  // Called when stream is closed
  pw::Callback<void()> on_closed_cb_;

  hci::CommandChannel::WeakPtr cmd_;

  hci::CommandChannel::EventHandlerId cis_established_handler_;

  WeakSelf<IsoStream> weak_self_;

  BT_DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(IsoStream);
};

}  // namespace bt::iso

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_H_

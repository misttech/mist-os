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
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/transport.h"

namespace bt::iso {

class IsoStream : public hci::IsoDataChannel::ConnectionInterface {
 public:
  virtual ~IsoStream() = default;

  // Handler for incoming HCI_LE_CIS_Established events. Returns a value
  // indicating whether the vent was handled.
  virtual bool OnCisEstablished(const hci::EmbossEventPacket& event) = 0;

  enum SetupDataPathError {
    kSuccess,
    kStreamAlreadyExists,
    kCisNotEstablished,
    kStreamRejectedByController,
    kInvalidArgs,
    kStreamClosed,
  };

  virtual void SetupDataPath(
      pw::bluetooth::emboss::DataPathDirection direction,
      const bt::StaticPacket<pw::bluetooth::emboss::CodecIdWriter>& codec_id,
      const std::optional<std::vector<uint8_t>>& codec_configuration,
      uint32_t controller_delay_usecs,
      fit::function<void(SetupDataPathError)> cb) = 0;

  virtual hci_spec::ConnectionHandle cis_handle() const = 0;

  // Terminate this stream.
  virtual void Close() = 0;

  static std::unique_ptr<IsoStream> Create(
      uint8_t cig_id,
      uint8_t cis_id,
      hci_spec::ConnectionHandle cis_handle,
      CisEstablishedCallback on_established_cb,
      hci::CommandChannel::WeakPtr cmd,
      pw::Callback<void()> on_closed_cb);

  using WeakPtr = WeakSelf<IsoStream>::WeakPtr;
  virtual WeakPtr GetWeakPtr() = 0;
};

}  // namespace bt::iso

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_ISO_STREAM_H_

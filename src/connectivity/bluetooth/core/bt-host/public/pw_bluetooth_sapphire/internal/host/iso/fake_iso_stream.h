// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_FAKE_ISO_STREAM_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_FAKE_ISO_STREAM_H_

#include <optional>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_stream.h"

namespace bt::iso::testing {

// Testing replacement for IsoStream with functionality built up as needed.
class FakeIsoStream : public IsoStream {
 public:
  FakeIsoStream() : weak_self_(this) {}

  bool OnCisEstablished(const hci::EmbossEventPacket& event) override {
    return true;
  }

  void SetupDataPath(
      pw::bluetooth::emboss::DataPathDirection direction,
      const bt::StaticPacket<pw::bluetooth::emboss::CodecIdWriter>& codec_id,
      const std::optional<std::vector<uint8_t>>& codec_configuration,
      uint32_t controller_delay_usecs,
      fit::function<void(IsoStream::SetupDataPathError)> cb) override {
    cb(setup_data_path_status_);
  }

  void ReceiveInboundPacket() override {}

  hci_spec::ConnectionHandle cis_handle() const override { return 0; }

  void Close() override {}

  IsoStream::WeakPtr GetWeakPtr() override { return weak_self_.GetWeakPtr(); }

  void SetSetupDataPathReturnStatus(IsoStream::SetupDataPathError status) {
    setup_data_path_status_ = status;
  }

 protected:
  IsoStream::SetupDataPathError setup_data_path_status_ =
      IsoStream::SetupDataPathError::kSuccess;

 private:
  WeakSelf<FakeIsoStream> weak_self_;
};

}  // namespace bt::iso::testing

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_ISO_FAKE_ISO_STREAM_H_

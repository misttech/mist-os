// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_ISO_STREAM_SERVER_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_ISO_STREAM_SERVER_H_

#include <fuchsia/bluetooth/le/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>

#include "src/connectivity/bluetooth/core/bt-host/fidl/server_base.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/weak_self.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_common.h"

namespace bthost {
class IsoStreamServer : public ServerBase<fuchsia::bluetooth::le::IsochronousStream> {
 public:
  explicit IsoStreamServer(
      fidl::InterfaceRequest<fuchsia::bluetooth::le::IsochronousStream> request,
      fit::callback<void()> on_closed_cb);

  void OnStreamEstablished(bt::iso::IsoStream::WeakPtr stream_ptr,
                           const bt::iso::CisEstablishedParameters& connection_params);

  void OnStreamEstablishmentFailed(pw::bluetooth::emboss::StatusCode status);

  void OnClosed();

  void Close(zx_status_t epitaph);

  using WeakPtr = WeakSelf<IsoStreamServer>::WeakPtr;
  WeakPtr GetWeakPtr() { return weak_self_.GetWeakPtr(); }

 private:
  // fuchsia::bluetooth::le::IsochronousStream overrides:
  void SetupDataPath(fuchsia::bluetooth::le::IsochronousStreamSetupDataPathRequest parameters,
                     SetupDataPathCallback callback) override;
  void Read(ReadCallback callback) override;
  void handle_unknown_method(uint64_t ordinal, bool has_response) override;

  fit::callback<void()> on_closed_cb_;

  std::optional<bt::iso::IsoStream::WeakPtr> iso_stream_;

  WeakSelf<IsoStreamServer> weak_self_;

  BT_DISALLOW_COPY_ASSIGN_AND_MOVE(IsoStreamServer);
};

}  // namespace bthost

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_FIDL_ISO_STREAM_SERVER_H_

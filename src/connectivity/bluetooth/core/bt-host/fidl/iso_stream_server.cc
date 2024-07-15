// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/fidl/iso_stream_server.h"

#include <lib/fidl/cpp/wire/channel.h>

#include <cinttypes>

#include "src/connectivity/bluetooth/core/bt-host/fidl/helpers.h"

namespace bthost {

IsoStreamServer::IsoStreamServer(
    fidl::InterfaceRequest<fuchsia::bluetooth::le::IsochronousStream> request,
    fit::callback<void()> on_closed_cb)
    : ServerBase(this, std::move(request)),
      on_closed_cb_(std::move(on_closed_cb)),
      weak_self_(this) {
  set_error_handler([this](zx_status_t) { OnClosed(); });
}

void IsoStreamServer::OnStreamEstablished(
    bt::iso::IsoStream::WeakPtr stream_ptr,
    const bt::iso::CisEstablishedParameters& connection_params) {
  bt_log(INFO, "fidl", "CIS established");
  iso_stream_ = stream_ptr;
  fuchsia::bluetooth::le::IsochronousStreamOnEstablishedRequest request;
  request.set_result(ZX_OK);
  fuchsia::bluetooth::le::CisEstablishedParameters params =
      bthost::fidl_helpers::CisEstablishedParametersToFidl(connection_params);
  request.set_established_params(std::move(params));
  binding()->events().OnEstablished(std::move(request));
}

void IsoStreamServer::OnStreamEstablishmentFailed(pw::bluetooth::emboss::StatusCode status) {
  BT_ASSERT(status != pw::bluetooth::emboss::StatusCode::SUCCESS);
  bt_log(WARN, "fidl", "CIS failed to be established: %u", static_cast<unsigned>(status));
  fuchsia::bluetooth::le::IsochronousStreamOnEstablishedRequest request;
  request.set_result(ZX_ERR_INTERNAL);
  binding()->events().OnEstablished(std::move(request));
}

void IsoStreamServer::SetupDataPath(
    fuchsia::bluetooth::le::IsochronousStreamSetupDataPathRequest parameters,
    SetupDataPathCallback callback) {
  callback(fpromise::error(ZX_ERR_NOT_SUPPORTED));
}

void IsoStreamServer::Read(ReadCallback callback) {}

void IsoStreamServer::OnClosed() {
  if (iso_stream_.has_value() && iso_stream_->is_alive()) {
    (*iso_stream_)->Close();
  }
  // This may free our instance.
  on_closed_cb_();
}

void IsoStreamServer::Close(zx_status_t epitaph) {
  binding()->Close(epitaph);
  OnClosed();
}

void IsoStreamServer::handle_unknown_method(uint64_t ordinal, bool has_response) {
  bt_log(WARN, "fidl", "Received unknown fidl call %#" PRIx64 " (%s responses)", ordinal,
         has_response ? "with" : "without");
}

}  // namespace bthost

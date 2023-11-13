// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite-server.h"

#include <lib/driver/component/cpp/driver_base.h>

namespace audio::aml_g12 {

// TODO(fxbug.dev/132252): Implement audio-composite server support.

void Server::Reset(ResetCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  completer.Reply(fuchsia_hardware_audio::HealthState{}.healthy(true));
}

void Server::SignalProcessingConnect(SignalProcessingConnectRequest& request,
                                     SignalProcessingConnectCompleter::Sync& completer) {
  if (signal_) {
    request.protocol().Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  signal_.emplace(dispatcher(), std::move(request.protocol()), this,
                  std::mem_fn(&Server::OnSignalProcessingClosed));
}

void Server::OnSignalProcessingClosed(fidl::UnbindInfo info) {
  if (info.is_peer_closed()) {
    FDF_LOG(INFO, "Client disconnected");
  } else if (!info.is_user_initiated()) {
    // Do not log canceled cases; these happen particularly frequently in certain test cases.
    if (info.status() != ZX_ERR_CANCELED) {
      FDF_LOG(ERROR, "Client connection unbound: %s", info.status_string());
    }
  }
  if (signal_) {
    signal_.reset();
  }
}

void Server::GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                                  GetRingBufferFormatsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::CreateRingBuffer(CreateRingBufferRequest& request,
                              CreateRingBufferCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetDaiFormats(GetDaiFormatsRequest& request, GetDaiFormatsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetProperties(
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::GetVmo(
    GetVmoRequest& request,
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetVmoCompleter::Sync& completer) {
  completer.Reply(zx::error(fuchsia_hardware_audio::GetVmoError::kInternalError));
}

void Server::Start(StartCompleter::Sync& completer) { completer.Reply({}); }

void Server::Stop(StopCompleter::Sync& completer) { completer.Reply(); }

void Server::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) { completer.Reply({}); }

void Server::SetActiveChannels(fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
                               SetActiveChannelsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetElements(GetElementsCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::WatchElementState(WatchElementStateRequest& request,
                               WatchElementStateCompleter::Sync& completer) {
  completer.Reply({});
}

void Server::SetElementState(SetElementStateRequest& request,
                             SetElementStateCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void Server::WatchTopology(WatchTopologyCompleter::Sync& completer) { completer.Reply({}); }

void Server::SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) {
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

}  // namespace audio::aml_g12

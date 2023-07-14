// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/ring_buffer_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/fit/internal/result.h>
#include <zircon/errors.h>

#include <optional>

#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

// static
std::shared_ptr<RingBufferServer> RingBufferServer::Create(
    std::shared_ptr<const FidlThread> thread,
    fidl::ServerEnd<fuchsia_audio_device::RingBuffer> server_end,
    std::shared_ptr<ControlServer> parent, std::shared_ptr<Device> device) {
  ADR_LOG_CLASS(kLogObjectLifetimes);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), parent, device);
}

RingBufferServer::RingBufferServer(std::shared_ptr<ControlServer> parent,
                                   std::shared_ptr<Device> device)
    : parent_(parent), device_(device) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

RingBufferServer::~RingBufferServer() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

// Called when the client drops the connection first.
void RingBufferServer::OnShutdown(fidl::UnbindInfo info) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  if (!info.is_peer_closed() && !info.is_user_initiated()) {
    ADR_WARN_OBJECT() << "shutdown with unexpected status: " << info;
  } else {
    ADR_LOG_OBJECT(kLogRingBufferServerResponses || kLogObjectLifetimes) << "with status: " << info;
  }

  if (!device_dropped_ring_buffer_) {
    device_->DropRingBuffer();

    // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
  }
}

void RingBufferServer::ClientDroppedControl() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);

  Shutdown(ZX_ERR_PEER_CLOSED);
  // Nothing else is needed: OnShutdown may call DropRingBuffer; our dtor will clear parent_.
}

// Called when the Device drops the RingBuffer FIDL.
void RingBufferServer::DeviceDroppedRingBuffer() {
  ADR_LOG_OBJECT(kLogRingBufferServerMethods || kLogNotifyMethods);

  device_dropped_ring_buffer_ = true;
  Shutdown(ZX_ERR_PEER_CLOSED);

  // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
  // Same for parent_ -- we want to ensure we destruct before our parent ControlServer.
}

// fuchsia.audio.device.RingBuffer implementation
//
void RingBufferServer::SetActiveChannels(SetActiveChannelsRequest& request,
                                         SetActiveChannelsCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRingBufferServerMethods);

  if (!request.channel_bitmask()) {
    FX_LOGS(WARNING) << kClassName << "::" << __func__
                     << ": required field 'channel_bitmask' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kInvalidChannelBitmask));
    return;
  }

  if (active_channels_completer_) {
    ADR_WARN_OBJECT() << "previous `SetActiveChannels` request has not yet completed";
    active_channels_completer_->Close(ZX_ERR_SHOULD_WAIT);
    return;
  }

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_OBJECT() << "device has an error";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kDeviceError));
    return;
  }

  // By this time, the Device should know whether the driver supports this method.
  FX_CHECK(device_->supports_set_active_channels().has_value());
  if (!*device_->supports_set_active_channels()) {
    ADR_LOG_OBJECT(kLogRingBufferServerMethods) << "device does not support SetActiveChannels";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kMethodNotSupported));
    return;
  }

  FX_CHECK(device_->ring_buffer_format().channel_count());
  if (*request.channel_bitmask() >= (1u << *device_->ring_buffer_format().channel_count())) {
    ADR_WARN_OBJECT() << "channel_bitmask (0x0" << std::hex << *request.channel_bitmask()
                      << ") too large, for this " << std::dec
                      << *device_->ring_buffer_format().channel_count() << "-channel format";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kChannelOutOfRange));
    return;
  }

  active_channels_completer_ = completer.ToAsync();
  device_->SetActiveChannels(*request.channel_bitmask(), [this](zx::result<zx::time> result) {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "Device/SetActiveChannels response";
    if (!active_channels_completer_) {
      ADR_WARN_OBJECT()
          << "active_channels_completer_ gone by the time the StartRingBuffer callback ran";
      return;
    }
    auto completer = std::move(active_channels_completer_);
    active_channels_completer_ = std::nullopt;

    if (result.is_error()) {
      completer->Reply(
          fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kDeviceError));
    }

    completer->Reply(fit::success(fuchsia_audio_device::RingBufferSetActiveChannelsResponse{{
        .set_time = result.value().get(),
    }}));
  });
}

void RingBufferServer::Start(StartRequest& request, StartCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRingBufferServerMethods);

  if (start_completer_) {
    ADR_WARN_OBJECT() << "previous `Start` request has not yet completed";
    start_completer_->Close(ZX_ERR_SHOULD_WAIT);
    return;
  }

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_OBJECT() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kDeviceError));
    return;
  }

  if (started_) {
    FX_LOGS(WARNING) << kClassName << "(" << this << ")::" << __func__
                     << ": device is already started";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kAlreadyStarted));
    return;
  }

  start_completer_ = completer.ToAsync();
  device_->StartRingBuffer([this](zx::result<zx::time> result) {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "Device/StartRingBuffer response";
    started_ = true;
    if (!start_completer_) {
      ADR_WARN_OBJECT() << "start_completer_ gone by the time the StartRingBuffer callback ran";
      return;
    }
    auto completer = std::move(start_completer_);
    start_completer_ = std::nullopt;
    if (result.is_error()) {
      completer->Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kDeviceError));
    }

    completer->Reply(fit::success(fuchsia_audio_device::RingBufferStartResponse{{
        .start_time = result.value().get(),
    }}));
  });
}

void RingBufferServer::Stop(StopRequest& request, StopCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRingBufferServerMethods);

  if (stop_completer_) {
    ADR_WARN_OBJECT() << "previous `Stop` request has not yet completed";
    stop_completer_->Close(ZX_ERR_SHOULD_WAIT);
    return;
  }

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_OBJECT() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kDeviceError));
    return;
  }

  if (!started_) {
    FX_LOGS(WARNING) << kClassName << "(" << this << ")::" << __func__ << ": device is not started";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kAlreadyStopped));
    return;
  }

  stop_completer_ = completer.ToAsync();
  device_->StopRingBuffer([this](zx_status_t status) {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "Device/StopRingBuffer response";
    started_ = false;
    if (!stop_completer_) {
      ADR_WARN_OBJECT() << "stop_completer_ gone by the time the StopRingBuffer callback ran";
      return;
    }
    auto completer = std::move(stop_completer_);
    stop_completer_ = std::nullopt;
    if (status != ZX_OK) {
      completer->Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kDeviceError));
      return;
    }

    completer->Reply(fit::success(fuchsia_audio_device::RingBufferStopResponse{}));
  });
}

void RingBufferServer::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogRingBufferServerMethods);

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_OBJECT() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferWatchDelayInfoError::kDeviceError));
    return;
  }

  if (delay_info_completer_) {
    ADR_WARN_OBJECT() << "previous `WatchDelayInfo` request has not yet completed";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferWatchDelayInfoError::kWatchAlreadyPending));
    return;
  }

  if (delay_info_update_) {
    completer.Reply(fit::success(fuchsia_audio_device::RingBufferWatchDelayInfoResponse{{
        .delay_info = *delay_info_update_,
    }}));
    delay_info_update_ = std::nullopt;
    return;
  }

  delay_info_completer_ = completer.ToAsync();
}

void RingBufferServer::DelayInfoChanged(const fuchsia_audio_device::DelayInfo& delay_info) {
  ADR_LOG_OBJECT(kLogRingBufferFidlResponses || kLogNotifyMethods);

  if (!delay_info_completer_) {
    delay_info_update_ = delay_info;
    return;
  }
  FX_CHECK(!delay_info_update_);

  delay_info_completer_->Reply(fit::success(fuchsia_audio_device::RingBufferWatchDelayInfoResponse{{
      .delay_info = delay_info,
  }}));
  delay_info_completer_ = std::nullopt;
}

}  // namespace media_audio

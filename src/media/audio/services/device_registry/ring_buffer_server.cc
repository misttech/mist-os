// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/ring_buffer_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/fit/internal/result.h>
#include <zircon/errors.h>

#include <optional>
#include <utility>

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
    std::shared_ptr<ControlServer> parent, std::shared_ptr<Device> device, ElementId element_id) {
  ADR_LOG_STATIC(kLogObjectLifetimes);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), std::move(parent),
                                std::move(device), element_id);
}

RingBufferServer::RingBufferServer(std::shared_ptr<ControlServer> parent,
                                   std::shared_ptr<Device> device, ElementId element_id)
    : parent_(std::move(parent)), device_(std::move(device)), element_id_(element_id) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

RingBufferServer::~RingBufferServer() {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

// Called when the client drops the connection first.
void RingBufferServer::OnShutdown(fidl::UnbindInfo info) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  if (!info.is_peer_closed() && !info.is_user_initiated()) {
    ADR_WARN_METHOD() << "shutdown with unexpected status: " << info;
  } else {
    ADR_LOG_METHOD(kLogRingBufferServerResponses || kLogObjectLifetimes) << "with status: " << info;
  }

  if (!device_dropped_ring_buffer_) {
    device_->DropRingBuffer(element_id_);

    // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
  }
}

void RingBufferServer::ClientDroppedControl() {
  ADR_LOG_METHOD(kLogObjectLifetimes);

  Shutdown(ZX_ERR_PEER_CLOSED);
  // Nothing else is needed: OnShutdown may call DropRingBuffer; our dtor will clear parent_.
}

// Called when the Device drops the RingBuffer FIDL.
void RingBufferServer::DeviceDroppedRingBuffer() {
  ADR_LOG_METHOD(kLogRingBufferServerMethods || kLogNotifyMethods);

  device_dropped_ring_buffer_ = true;
  Shutdown(ZX_ERR_PEER_CLOSED);

  // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
  // Same for parent_ -- we want to ensure we destruct before our parent ControlServer.
}

// fuchsia.audio.device.RingBuffer implementation
//
void RingBufferServer::SetActiveChannels(SetActiveChannelsRequest& request,
                                         SetActiveChannelsCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogRingBufferServerMethods);

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kDeviceError));
    return;
  }

  if (active_channels_completer_) {
    ADR_WARN_METHOD() << "previous `SetActiveChannels` request has not yet completed";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kAlreadyPending));
    return;
  }

  // By this time, the Device should know whether the driver supports this method.
  FX_CHECK(device_->supports_set_active_channels(element_id_).has_value());
  if (!*device_->supports_set_active_channels(element_id_)) {
    ADR_LOG_METHOD(kLogRingBufferServerMethods) << "device does not support SetActiveChannels";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kMethodNotSupported));
    return;
  }

  if (!request.channel_bitmask()) {
    ADR_WARN_METHOD() << "required field 'channel_bitmask' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kInvalidChannelBitmask));
    return;
  }

  FX_CHECK(device_->ring_buffer_format(element_id_).channel_count());
  if (*request.channel_bitmask() >=
      (1u << *device_->ring_buffer_format(element_id_).channel_count())) {
    ADR_WARN_METHOD() << "channel_bitmask (0x0" << std::hex << *request.channel_bitmask()
                      << ") too large, for this " << std::dec
                      << *device_->ring_buffer_format(element_id_).channel_count()
                      << "-channel format";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kChannelOutOfRange));
    return;
  }

  active_channels_completer_ = completer.ToAsync();
  auto succeeded = device_->SetActiveChannels(
      element_id_, *request.channel_bitmask(), [this](zx::result<zx::time> result) {
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "Device/SetActiveChannels response";
        // If we have no async completer, maybe we're shutting down and it was cleared. Just exit.
        if (!active_channels_completer_) {
          ADR_WARN_OBJECT()
              << "active_channels_completer_ gone by the time the StartRingBuffer callback ran";
          return;
        }

        auto completer = std::move(active_channels_completer_);
        active_channels_completer_.reset();
        if (result.is_error()) {
          ADR_WARN_OBJECT() << "SetActiveChannels callback: device has an error";
          completer->Reply(
              fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kDeviceError));
        }

        completer->Reply(fit::success(fuchsia_audio_device::RingBufferSetActiveChannelsResponse{{
            .set_time = result.value().get(),
        }}));
      });

  // Should be prevented by the `supports_set_active_channels` check above, but if Device returns
  // false, it's because the element returned NOT_SUPPORTED from a previous SetActiveChannels.
  if (!succeeded) {
    ADR_LOG_METHOD(kLogRingBufferServerMethods) << "device does not support SetActiveChannels";
    auto completer = std::move(active_channels_completer_);
    active_channels_completer_.reset();
    completer->Reply(
        fit::error(fuchsia_audio_device::RingBufferSetActiveChannelsError::kMethodNotSupported));
  }

  // Otherwise, `active_channels_completer_` is saved for the future async response.
}

void RingBufferServer::Start(StartRequest& request, StartCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogRingBufferServerMethods);

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kDeviceError));
    return;
  }

  if (start_completer_) {
    ADR_WARN_METHOD() << "previous `Start` request has not yet completed";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kAlreadyPending));
    return;
  }

  if (started_) {
    ADR_WARN_METHOD() << "device is already started";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kAlreadyStarted));
    return;
  }

  start_completer_ = completer.ToAsync();
  device_->StartRingBuffer(element_id_, [this](zx::result<zx::time> result) {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "Device/StartRingBuffer response";
    // If we have no async completer, maybe we're shutting down and it was cleared. Just exit.
    if (!start_completer_) {
      ADR_WARN_OBJECT() << "start_completer_ gone by the time the StartRingBuffer callback ran";
      return;
    }

    auto completer = std::move(start_completer_);
    start_completer_.reset();
    if (result.is_error()) {
      ADR_WARN_OBJECT() << "Start callback: device has an error";
      completer->Reply(fit::error(fuchsia_audio_device::RingBufferStartError::kDeviceError));
    }

    started_ = true;
    completer->Reply(fit::success(fuchsia_audio_device::RingBufferStartResponse{{
        .start_time = result.value().get(),
    }}));
  });
}

void RingBufferServer::Stop(StopRequest& request, StopCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogRingBufferServerMethods);

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kDeviceError));
    return;
  }

  if (stop_completer_) {
    ADR_WARN_METHOD() << "previous `Stop` request has not yet completed";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kAlreadyPending));
    return;
  }

  if (!started_) {
    ADR_WARN_METHOD() << "device is not started";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kAlreadyStopped));
    return;
  }

  stop_completer_ = completer.ToAsync();
  device_->StopRingBuffer(element_id_, [this](zx_status_t status) {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "Device/StopRingBuffer response";
    if (!stop_completer_) {
      // If we have no async completer, maybe we're shutting down and it was cleared. Just exit.
      ADR_WARN_OBJECT() << "stop_completer_ gone by the time the StopRingBuffer callback ran";
      return;
    }

    auto completer = std::move(stop_completer_);
    stop_completer_.reset();
    if (status != ZX_OK) {
      ADR_WARN_OBJECT() << "Stop callback: device has an error";
      completer->Reply(fit::error(fuchsia_audio_device::RingBufferStopError::kDeviceError));
      return;
    }

    started_ = false;
    completer->Reply(fit::success(fuchsia_audio_device::RingBufferStopResponse{}));
  });
}

void RingBufferServer::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogRingBufferServerMethods);

  if (parent_->ControlledDeviceReceivedError()) {
    ADR_WARN_METHOD() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::RingBufferWatchDelayInfoError::kDeviceError));
    return;
  }

  if (delay_info_completer_) {
    ADR_WARN_METHOD() << "previous `WatchDelayInfo` request has not yet completed";
    completer.Reply(
        fit::error(fuchsia_audio_device::RingBufferWatchDelayInfoError::kAlreadyPending));
    return;
  }

  delay_info_completer_ = completer.ToAsync();
  MaybeCompleteWatchDelayInfo();
}

void RingBufferServer::DelayInfoChanged(const fuchsia_audio_device::DelayInfo& delay_info) {
  ADR_LOG_METHOD(kLogRingBufferFidlResponses || kLogNotifyMethods);

  new_delay_info_to_notify_ = delay_info;
  MaybeCompleteWatchDelayInfo();
}

void RingBufferServer::MaybeCompleteWatchDelayInfo() {
  if (new_delay_info_to_notify_ && delay_info_completer_) {
    auto delay_info = *new_delay_info_to_notify_;
    new_delay_info_to_notify_.reset();

    auto completer = std::move(*delay_info_completer_);
    delay_info_completer_.reset();

    completer.Reply(fit::success(fuchsia_audio_device::RingBufferWatchDelayInfoResponse{{
        .delay_info = delay_info,
    }}));
  }
}

}  // namespace media_audio

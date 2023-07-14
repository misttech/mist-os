// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/observer_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fit/internal/result.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

// static
std::shared_ptr<ObserverServer> ObserverServer::Create(
    std::shared_ptr<const FidlThread> thread,
    fidl::ServerEnd<fuchsia_audio_device::Observer> server_end,
    std::shared_ptr<const Device> device) {
  ADR_LOG_CLASS(kLogObserverServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), device);
}

ObserverServer::ObserverServer(std::shared_ptr<const Device> device) : device_(device) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);

  // TODO(fxbug/dev:117199): When Health can change post-initialization, consider checking Health.

  ++count_;
  LogObjectCounts();
}

ObserverServer::~ObserverServer() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

void ObserverServer::DeviceHasError() {
  ADR_LOG_OBJECT(kLogObserverServerMethods || kLogNotifyMethods);

  has_error_ = true;
  DeviceIsRemoved();
}

// Called when the Device shuts down first.
void ObserverServer::DeviceIsRemoved() {
  ADR_LOG_OBJECT(kLogObserverServerMethods || kLogNotifyMethods);

  Shutdown(ZX_ERR_PEER_CLOSED);

  // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
}

void ObserverServer::GainStateChanged(const fuchsia_audio_device::GainState& new_gain_state) {
  ADR_LOG_OBJECT(kLogObserverServerMethods || kLogNotifyMethods);

  if (watch_gain_state_completer_) {
    updated_gain_state_.reset();

    auto completer = std::move(*watch_gain_state_completer_);
    watch_gain_state_completer_.reset();
    completer.Reply(fit::success(fuchsia_audio_device::ObserverWatchGainStateResponse{{
        .state = new_gain_state,
    }}));
  } else {
    updated_gain_state_ = new_gain_state;
  }
}

void ObserverServer::WatchGainState(WatchGainStateCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogObserverServerMethods);

  if (has_error_) {
    completer.Reply(fit::error<fuchsia_audio_device::ObserverWatchGainStateError>(
        fuchsia_audio_device::ObserverWatchGainStateError::kDeviceError));
    return;
  }
  if (watch_gain_state_completer_) {
    ADR_WARN_OBJECT() << "previous `WatchGainState` request has not yet completed";
    completer.Reply(fit::error<fuchsia_audio_device::ObserverWatchGainStateError>(
        fuchsia_audio_device::ObserverWatchGainStateError::kWatchAlreadyPending));
    return;
  }

  if (updated_gain_state_) {
    fuchsia_audio_device::ObserverWatchGainStateResponse response{{
        .state = std::move(*updated_gain_state_),
    }};
    updated_gain_state_.reset();
    completer.Reply(fit::success(std::move(response)));
  } else {
    watch_gain_state_completer_ = completer.ToAsync();
  }
}

void ObserverServer::PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                                      zx::time plug_change_time) {
  ADR_LOG_OBJECT(kLogObserverServerMethods || kLogNotifyMethods)
      << new_plug_state << " @ " << plug_change_time.get();

  plug_state_update_ = fuchsia_audio_device::ObserverWatchPlugStateResponse{{
      .state = new_plug_state,
      .plug_time = plug_change_time.get(),
  }};

  if (watch_plug_state_completer_) {
    auto completer = std::move(*watch_plug_state_completer_);
    watch_plug_state_completer_.reset();

    fuchsia_audio_device::ObserverWatchPlugStateResponse response = std::move(*plug_state_update_);
    plug_state_update_.reset();
    completer.Reply(fit::success(response));
  }
}

void ObserverServer::WatchPlugState(WatchPlugStateCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogObserverServerMethods);

  if (has_error_) {
    completer.Reply(fit::error<fuchsia_audio_device::ObserverWatchPlugStateError>(
        fuchsia_audio_device::ObserverWatchPlugStateError::kDeviceError));
    return;
  }
  if (watch_plug_state_completer_) {
    ADR_WARN_OBJECT() << "previous `WatchPlugState` request has not yet completed";
    completer.Reply(fit::error<fuchsia_audio_device::ObserverWatchPlugStateError>(
        fuchsia_audio_device::ObserverWatchPlugStateError::kWatchAlreadyPending));
    return;
  }

  if (plug_state_update_) {
    fuchsia_audio_device::ObserverWatchPlugStateResponse response = std::move(*plug_state_update_);
    plug_state_update_.reset();
    completer.Reply(fit::success(response));
  } else {
    watch_plug_state_completer_ = completer.ToAsync();
  }
}

void ObserverServer::GetReferenceClock(GetReferenceClockCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogObserverServerMethods);

  if (has_error_) {
    completer.Reply(fit::error<fuchsia_audio_device::ObserverGetReferenceClockError>(
        fuchsia_audio_device::ObserverGetReferenceClockError::kDeviceError));
    return;
  }

  FX_CHECK(device_);
  auto clock_result = device_->GetReadOnlyClock();
  if (clock_result.is_error()) {
    completer.Reply(fit::error<fuchsia_audio_device::ObserverGetReferenceClockError>(
        fuchsia_audio_device::ObserverGetReferenceClockError::kDeviceClockUnavailable));
    return;
  }
  fuchsia_audio_device::ObserverGetReferenceClockResponse response = {{
      .reference_clock = std::move(clock_result.value()),
  }};
  completer.Reply(fit::success(std::move(response)));
}

}  // namespace media_audio

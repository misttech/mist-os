// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mem/cpp/natural_types.h>
#include <lib/fidl/cpp/wire/status.h>
#include <lib/fit/internal/result.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>

#include <iomanip>
#include <limits>
#include <optional>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {

// static
std::shared_ptr<ControlServer> ControlServer::Create(
    std::shared_ptr<const FidlThread> thread,
    fidl::ServerEnd<fuchsia_audio_device::Control> server_end,
    std::shared_ptr<AudioDeviceRegistry> parent, std::shared_ptr<Device> device) {
  ADR_LOG_CLASS(kLogControlServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), parent, device);
}

ControlServer::ControlServer(std::shared_ptr<AudioDeviceRegistry> parent,
                             std::shared_ptr<Device> device)
    : parent_(parent), device_(device) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

ControlServer::~ControlServer() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

// Called when the client shuts down first.
void ControlServer::OnShutdown(fidl::UnbindInfo info) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  if (!info.is_peer_closed() && !info.is_user_initiated()) {
    ADR_WARN_OBJECT() << "shutdown with unexpected status: " << info;
  } else {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses || kLogObjectLifetimes) << "with status: " << info;
  }

  if (auto ring_buffer = GetRingBufferServer(); ring_buffer) {
    ring_buffer->ClientDroppedControl();
    ring_buffer_server_ = std::nullopt;
  }
}

// Called when Device drops its RingBuffer FIDL. Tell RingBufferServer and drop our reference.
void ControlServer::DeviceDroppedRingBuffer() {
  ADR_LOG_OBJECT(kLogControlServerMethods || kLogNotifyMethods);

  if (auto ring_buffer = GetRingBufferServer(); ring_buffer) {
    ring_buffer->DeviceDroppedRingBuffer();
    ring_buffer_server_ = std::nullopt;
  }
}

void ControlServer::DeviceHasError() {
  ADR_LOG_OBJECT(kLogControlServerMethods);

  device_has_error_ = true;
  DeviceIsRemoved();
}

// Upon exiting this method, we drop our connection to the client.
void ControlServer::DeviceIsRemoved() {
  ADR_LOG_OBJECT(kLogControlServerMethods);

  if (auto ring_buffer = GetRingBufferServer(); ring_buffer) {
    ring_buffer->ClientDroppedControl();
    ring_buffer_server_ = std::nullopt;

    // We don't explicitly clear our shared_ptr<Device> reference, to ensure we destruct first.
  }
  Shutdown(ZX_ERR_PEER_CLOSED);
}

std::shared_ptr<RingBufferServer> ControlServer::GetRingBufferServer() {
  ADR_LOG_OBJECT(kLogControlServerMethods);
  if (ring_buffer_server_) {
    if (auto sh_ptr_ring_buffer_server = ring_buffer_server_->lock(); sh_ptr_ring_buffer_server) {
      return sh_ptr_ring_buffer_server;
    }
    ring_buffer_server_ = std::nullopt;
  }
  return nullptr;
}

// fuchsia.audio.device.Control implementation
void ControlServer::SetGain(SetGainRequest& request, SetGainCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogControlServerMethods);

  if (!request.target_state()) {
    ADR_WARN_OBJECT() << "required field 'target_state' is missing";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kInvalidGainState));
    return;
  }

  if (device_has_error_) {
    ADR_WARN_OBJECT() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kDeviceError));
    return;
  }

  auto& gain_caps = *device_->info()->gain_caps();
  if (!request.target_state()->gain_db()) {
    ADR_WARN_OBJECT() << "required field `target_state.gain_db` is missing";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kInvalidGainDb));
    return;
  }

  if (*request.target_state()->gain_db() > *gain_caps.max_gain_db() ||
      *request.target_state()->gain_db() < *gain_caps.min_gain_db()) {
    ADR_WARN_OBJECT() << "gain_db (" << *request.target_state()->gain_db() << ") is out of range ["
                      << *gain_caps.min_gain_db() << ", " << *gain_caps.max_gain_db() << "]";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kGainOutOfRange));
    return;
  }

  if (request.target_state()->muted().value_or(false) && !(*gain_caps.can_mute())) {
    ADR_WARN_OBJECT() << "device cannot MUTE";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kMuteUnavailable));
    return;
  }

  if (request.target_state()->agc_enabled().value_or(false) && !(*gain_caps.can_agc())) {
    ADR_WARN_OBJECT() << "device cannot AGC";
    completer.Reply(fit::error(fuchsia_audio_device::ControlSetGainError::kAgcUnavailable));
    return;
  }

  fuchsia_hardware_audio::GainState gain_state{{.gain_db = *request.target_state()->gain_db()}};
  if (request.target_state()->muted()) {
    gain_state.muted(*request.target_state()->muted());
  }
  if (request.target_state()->agc_enabled()) {
    gain_state.agc_enabled(*request.target_state()->agc_enabled());
  }
  device_->SetGain(gain_state);

  completer.Reply(fit::success(fuchsia_audio_device::ControlSetGainResponse{}));
}

void ControlServer::GetCurrentlyPermittedFormats(
    GetCurrentlyPermittedFormatsCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogControlServerMethods);

  currently_permitted_formats_completer_ = completer.ToAsync();
  device_->GetCurrentlyPermittedFormats(
      [this](std::vector<fuchsia_audio_device::PcmFormatSet> formats) {
        // If we have no async completer, maybe we're shutting down and it was cleared. Just exit.
        if (!currently_permitted_formats_completer_) {
          return;
        }

        auto completer = std::move(currently_permitted_formats_completer_);
        currently_permitted_formats_completer_ = std::nullopt;
        if (device_has_error_) {
          ADR_WARN_OBJECT() << "device has an error";
          completer->Reply(fit::error(
              fuchsia_audio_device::ControlGetCurrentlyPermittedFormatsError::kDeviceError));
          return;
        }

        FX_CHECK(!formats.empty());
        completer->Reply(
            fit::success(fuchsia_audio_device::ControlGetCurrentlyPermittedFormatsResponse{{
                .permitted_formats = std::move(formats),
            }}));
      });
}

void ControlServer::CreateRingBuffer(CreateRingBufferRequest& request,
                                     CreateRingBufferCompleter::Sync& completer) {
  ADR_LOG_OBJECT(kLogControlServerMethods);

  // Fail on missing parameters.
  if (!request.options()) {
    ADR_WARN_OBJECT() << "required field 'options' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidOptions));
    return;
  }
  if (!request.options()->format() || !request.options()->format()->sample_type() ||
      !request.options()->format()->channel_count() ||
      !request.options()->format()->frames_per_second()) {
    ADR_WARN_OBJECT() << "required 'options.format' (or one of its required members) is missing";
    completer.Reply(fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat));
    return;
  }
  if (!request.options()->ring_buffer_min_bytes()) {
    ADR_WARN_OBJECT() << "required field 'options.ring_buffer_min_bytes' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidMinBytes));
    return;
  }
  if (!request.ring_buffer_server()) {
    ADR_WARN_OBJECT() << "required field 'ring_buffer_server' is missing";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidRingBuffer));
    return;
  }
  // Fail if device has error.
  if (device_has_error_) {
    ADR_WARN_OBJECT() << "device has an error";
    completer.Reply(fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kDeviceError));
    return;
  }

  if (GetRingBufferServer()) {
    ADR_WARN_OBJECT() << "device RingBuffer already exists";
    completer.Reply(fit::error(
        fuchsia_audio_device::wire::ControlCreateRingBufferError::kRingBufferAlreadyAllocated));
  }

  auto driver_format = device_->SupportedDriverFormatForClientFormat(*request.options()->format());
  // Fail if device cannot satisfy the requested format.
  if (!driver_format) {
    ADR_WARN_OBJECT() << "device does not support the specified options";
    completer.Reply(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch));
    return;
  }

  create_ring_buffer_completer_ = completer.ToAsync();

  bool created = device_->CreateRingBuffer(
      *driver_format, *request.options()->ring_buffer_min_bytes(),
      [this](Device::RingBufferInfo info) {
        // If we have no async completer, maybe we're shutting down. Just exit.
        if (!create_ring_buffer_completer_) {
          if (auto ring_buffer_server = GetRingBufferServer(); ring_buffer_server) {
            ring_buffer_server_ = std::nullopt;
          }
          return;
        }

        auto completer = std::move(*create_ring_buffer_completer_);
        create_ring_buffer_completer_ = std::nullopt;

        completer.Reply(fit::success(fuchsia_audio_device::ControlCreateRingBufferResponse{{
            .properties = info.properties,
            .ring_buffer = std::move(info.ring_buffer),
        }}));
      });

  if (!created) {
    ADR_WARN_OBJECT() << "device cannot create a ring buffer with the specified options";
    ring_buffer_server_ = std::nullopt;
    create_ring_buffer_completer_->Reply(
        fidl::Response<fuchsia_audio_device::Control::CreateRingBuffer>(
            fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kBadRingBufferOption)));
    return;
  }
  auto ring_buffer_server = RingBufferServer::Create(
      thread_ptr(), std::move(*request.ring_buffer_server()), shared_from_this(), device_);
  AddChildServer(ring_buffer_server);
  ring_buffer_server_ = ring_buffer_server;
}

void ControlServer::GainStateChanged(const fuchsia_audio_device::GainState&) {}

void ControlServer::PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                                     zx::time plug_change_time) {}

// We receive delay values for the first time during the configuration process. Once we have these
// values, we can calculate the required ring-buffer size and request the VMO.
void ControlServer::DelayInfoChanged(const fuchsia_audio_device::DelayInfo& delay_info) {
  ADR_LOG_OBJECT(kLogControlServerResponses || kLogNotifyMethods);

  // Initialization is complete, so this represents a delay update. Eventually, notify watchers.
  if (auto ring_buffer_server = GetRingBufferServer(); ring_buffer_server) {
    ring_buffer_server->DelayInfoChanged(delay_info);
  }
  delay_info_ = delay_info;
}

}  // namespace media_audio

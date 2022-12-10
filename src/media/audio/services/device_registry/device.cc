// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mediastreams/cpp/common_types.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <algorithm>
#include <cstdint>
#include <memory>
#include <optional>
#include <string_view>

#include "src/media/audio/lib/clock/real_clock.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

uint64_t NextTokenId() {
  static uint64_t token_id = 0;
  return token_id++;
}

// statics
uint32_t Device::count_ = 0;
uint32_t Device::initialized_count_ = 0;
uint32_t Device::unhealthy_count_ = 0;

std::shared_ptr<Device> Device::Create(
    std::weak_ptr<DevicePresenceWatcher> presence_watcher, async_dispatcher_t* dispatcher,
    std::string_view name, fuchsia_audio_device::DeviceType device_type,
    fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config) {
  ADR_LOG_CLASS(kLogObjectLifetimes);

  // The constructor is private, forcing clients to use Device::Create().
  class MakePublicCtor : public Device {
   public:
    MakePublicCtor(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
                   async_dispatcher_t* dispatcher, std::string_view name,
                   fuchsia_audio_device::DeviceType device_type,
                   fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config)
        : Device(std::move(presence_watcher), dispatcher, name, device_type,
                 std::move(stream_config)) {}
  };

  return std::make_shared<MakePublicCtor>(presence_watcher, dispatcher, name, device_type,
                                          std::move(stream_config));
}

// Device notifies presence_watcher when it is available (Ready), unhealthy (Error) or removed.
// The dispatcher member is needed for Device to create client connections to protocols such as
// fuchsia.hardware.audio.signalprocessing.Reader or fuchsia.hardware.audio.RingBuffer.
Device::Device(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
               async_dispatcher_t* dispatcher, std::string_view name,
               fuchsia_audio_device::DeviceType device_type,
               fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config)
    : presence_watcher_(std::move(presence_watcher)),
      dispatcher_(dispatcher),
      name_(name),
      device_type_(device_type),
      stream_config_(fidl::Client(std::move(stream_config), dispatcher, this)),
      token_id_(NextTokenId()) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  ++Device::count_;
  LogObjectCounts();

  // Upon creation, automatically kick off initialization. This will complete asynchronously,
  // notifying the presence_watcher at that time by calling DeviceIsReady().
  Initialize();
}

Device::~Device() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  --Device::count_;
  LogObjectCounts();
}

// Unwind any operational state or configuration the device might be in, and remove this device.
void Device::OnError(zx_status_t error) {
  ADR_LOG_OBJECT(kLogDeviceState);
  if (state_ == State::Error) {
    FX_LOGS(WARNING) << __func__ << ": device already has an error; ignoring subsequent error ("
                     << error << ")";
    return;
  }

  FX_PLOGS(WARNING, error) << __func__;

  ++Device::unhealthy_count_;
  SetStateError(error);
  LogObjectCounts();

  if (std::shared_ptr<DevicePresenceWatcher> pw = presence_watcher_.lock()) {
    pw->DeviceHasError(shared_from_this());
  }
}

// We received a successful (healthy) response from GetHealthState.
// This could occur during initialization, or later during normal operation.
void Device::OnHealthResponse() {
  ADR_LOG_OBJECT(kLogDeviceState || kLogStreamConfigFidlResponses);
  FX_CHECK(health_state_) << "Received " << __func__ << " but health_state_ was not set";

  // If device state is Initializing, this might be the final response we are waiting for.
  if (state_ == State::Initializing) {
    OnInitializationResponse();
    return;
  }
  // Otherwise, we asked for, and received, health status after initialization (subsequent CL).
  // TODO: potentially add DeviceIsHealthy to Notify interfaces, to notify observers/control.
}

// An initialization command returned a successful response. Is initialization complete?
void Device::OnInitializationResponse() {
  if (state_ == State::Error) {
    FX_LOGS(WARNING) << __func__ << ": device has already encountered a problem; ignoring this";
    return;
  }

  ADR_LOG_OBJECT(kLogDeviceInitializationProgress)
      << " (RECEIVED|pending)    "                                  //
      << (stream_config_properties_ ? "PROPS" : "props") << "    "  //
      << (formats_ ? "FORMATS" : "formats") << "    "               //
      << (gain_state_ ? "GAIN" : "gain") << "      "                //
      << (plug_state_ ? "PLUG" : "plug") << "     "                 //
      << (health_state_ ? "HEALTH" : "health");

  if (state_ != State::Initializing) {
    FX_LOGS(WARNING) << "Unexpected device initialization response when not Initializing";
  }

  if (stream_config_properties_ && formats_ && gain_state_ && plug_state_ && health_state_) {
    ++Device::initialized_count_;
    SetDeviceInfo();
    SetStateReady();
    LogObjectCounts();

    if (std::shared_ptr<DevicePresenceWatcher> pw = presence_watcher_.lock()) {
      pw->DeviceIsReady(shared_from_this());
    }
  }
}

void Device::SetStateError(zx_status_t error) {
  ADR_LOG_OBJECT(kLogDeviceState) << ": " << error;
  state_ = State::Error;
}

void Device::SetStateReady() {
  if (state_ == State::Error) {
    FX_LOGS(WARNING) << __func__ << ": device already has an error; ignoring this";
    return;
  }

  ADR_LOG_OBJECT(kLogDeviceState);
  state_ = State::Ready;
}

void Device::Initialize() {
  ADR_LOG_OBJECT(kLogDeviceMethods);

  QueryStreamProperties();
  QuerySupportedFormats();
  QueryGainState();
  QueryPlugState();
  QueryHealthState();
}

// This also sets OnError, so it is not just for logging.
template <typename ResultT>
bool Device::LogResultError(const ResultT& result, const char* debug_context) {
  if (state_ == State::Error) {
    FX_LOGS(WARNING) << debug_context << ": device already has an error; ignoring this";
    return true;
  }
  if (!result.is_ok()) {
    if (result.error_value().is_canceled() || result.error_value().is_dispatcher_shutdown() ||
        result.error_value().is_peer_closed()) {
      ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << debug_context << ": will take no action on "
                                                    << result.error_value().FormatDescription();
    } else {
      FX_LOGS(ERROR) << debug_context << " failed: " << result.error_value().status() << " ("
                     << result.error_value().FormatDescription() << ")";
      OnError(result.error_value().status());
    }
    return true;
  }
  return false;
}

void Device::QueryStreamProperties() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  // TODO(fxbug.dev/113429): handle command timeouts

  stream_config_->GetProperties().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetProperties>& result) {
        if (LogResultError(result, "GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "StreamConfig/GetProperties: success";
        auto status = ValidateStreamProperties(result->properties());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        FX_CHECK(!stream_config_properties_)
            << "StreamCOnfig/GetProperties response: stream_config_properties_ already set";
        stream_config_properties_ = result->properties();
        // We have our clock domain now. Create the device clock.
        CreateDeviceClock();

        OnInitializationResponse();
      });
}

void Device::QuerySupportedFormats() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  // TODO(fxbug.dev/113429): handle command timeouts

  stream_config_->GetSupportedFormats().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetSupportedFormats>& result) {
        if (LogResultError(result, "SupportedFormats response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
            << "StreamConfig/GetSupportedFormats: success";
        auto status = ValidateSupportedFormats(result->supported_formats());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        if (!formats_) {
          formats_ = std::vector<fuchsia_hardware_audio::SupportedFormats>();
          for (const auto& supported_formats : result->supported_formats()) {
            formats_->emplace_back(supported_formats);
          }
          OnInitializationResponse();
        }
        // otherwise, this is a GetCurrentlyPermittedFormats request (subsequent CL).
      });
}

void Device::QueryGainState() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  if (state_ == State::Initializing) {
    // TODO(fxbug.dev/113429): handle command timeouts (but not on subsequent watches)
  }

  stream_config_->WatchGainState().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::WatchGainState>& result) {
        if (LogResultError(result, "GainState response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchGainState response";
        auto status = ValidateGainState(result->gain_state());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        if (!gain_state_) {
          ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchGainState received initial value";

          gain_state_ = result->gain_state();
          gain_state_->muted() = gain_state_->muted().value_or(false);
          gain_state_->agc_enabled() = gain_state_->agc_enabled().value_or(false);
          OnInitializationResponse();
        }
        // Otherwise, we watched for, and received, a change in the gain state (subsequent CL).
      });
}

void Device::QueryPlugState() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  if (state_ == State::Initializing) {
    // TODO(fxbug.dev/113429): handle command timeouts (but not on subsequent watches)
  }

  stream_config_->WatchPlugState().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::WatchPlugState>& result) {
        if (LogResultError(result, "PlugState response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchPlugState response";
        auto status = ValidatePlugState(result->plug_state(), stream_config_properties_);
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        if (!plug_state_) {
          ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchPlugState received initial value";

          plug_state_ = result->plug_state();
          OnInitializationResponse();
        }
        // Otherwise, we watched for, and received, a change in the plug state (subsequent CL).
      });
}

void Device::QueryHealthState() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }

  // TODO(fxbug.dev/113429): handle command timeouts

  stream_config_->GetHealthState().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetHealthState>& result) {
        if (LogResultError(result, "HealthState response")) {
          return;
        }

        // An empty health state is permitted; it still indicates that the driver is responsive.
        health_state_ = {{
            .healthy = result->state().healthy().value_or(true),
        }};
        // ...but if the driver actually self-reported as unhealthy, this is a problem.
        if (!*health_state_->healthy()) {
          FX_LOGS(WARNING) << "QueryHealthState response: .healthy is FALSE (unhealthy)";
          OnError(ZX_ERR_IO);
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "QueryHealthState response: healthy";
        OnHealthResponse();
      });
}

// Return a fuchsia_audio_device::Info object based on this device's member values.
// Required fields (guaranteed for the caller) include: token_id, device_type, device_name,
// supported_formats, gain_caps, plug_detect_caps, clock_domain.
// Optional fields (not required from the driver): manufacturer, product, unique_instance_id.
fuchsia_audio_device::Info Device::CreateDeviceInfo() {
  ADR_LOG_OBJECT(kLogDeviceMethods);

  return {{
      .token_id = token_id_,
      .device_type = device_type_,
      .device_name = name_,

      .manufacturer = stream_config_properties_->manufacturer(),     // optional
      .product = stream_config_properties_->product(),               // optional
      .unique_instance_id = stream_config_properties_->unique_id(),  // optional

      .supported_formats = TranslateFormatSets(*formats_),
      .gain_caps = fuchsia_audio_device::GainCapabilities{{
          .min_gain_db = stream_config_properties_->min_gain_db(),
          .max_gain_db = stream_config_properties_->max_gain_db(),
          .gain_step_db = stream_config_properties_->gain_step_db(),
          .can_mute = stream_config_properties_->can_mute(),
          .can_agc = stream_config_properties_->can_agc(),
      }},
      .plug_detect_caps = (*stream_config_properties_->plug_detect_capabilities() ==
                                   fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired
                               ? fuchsia_audio_device::PlugDetectCapabilities::kHardwired
                               : fuchsia_audio_device::PlugDetectCapabilities::kPluggable),
      .clock_domain = stream_config_properties_->clock_domain(),
  }};
}

void Device::SetDeviceInfo() {
  ADR_LOG_OBJECT(kLogDeviceMethods);

  device_info_ = CreateDeviceInfo();
  ValidateDeviceInfo(*device_info_);
}

void Device::CreateDeviceClock() {
  ADR_LOG_OBJECT(kLogDeviceMethods);
  FX_CHECK(stream_config_properties_->clock_domain()) << "Clock domain is required";

  device_clock_ = RealClock::CreateFromMonotonic("'" + name_ + "' device clock",
                                                 *stream_config_properties_->clock_domain(),
                                                 (*stream_config_properties_->clock_domain() !=
                                                  fuchsia_hardware_audio::kClockDomainMonotonic));
}

// Create a duplicate handle to our clock with limited rights. We can transfer it to a client who
// can only read and duplicate. Specifically, they cannot change this clock's rate or offset.
zx::result<zx::clock> Device::GetReadOnlyClock() {
  ADR_LOG_OBJECT(kLogDeviceMethods);

  auto dupe_clock = device_clock_->DuplicateZxClockReadOnly();
  if (!dupe_clock) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  return zx::ok(std::move(*dupe_clock));
}

}  // namespace media_audio

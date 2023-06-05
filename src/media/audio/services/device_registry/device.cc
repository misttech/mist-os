// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fit/function.h>
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
#include "src/media/audio/lib/timeline/timeline_rate.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/observer_notify.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

uint64_t NextTokenId() {
  static uint64_t token_id = 0;
  return token_id++;
}

// static
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

// Device notifies presence_watcher when it is available (DeviceInitialized), unhealthy (Error) or
// removed. The dispatcher member is needed for Device to create client connections to protocols
// such as fuchsia.hardware.audio.signalprocessing.Reader or fuchsia.hardware.audio.RingBuffer.
Device::Device(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
               async_dispatcher_t* dispatcher, std::string_view name,
               fuchsia_audio_device::DeviceType device_type,
               fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config)
    : presence_watcher_(std::move(presence_watcher)),
      dispatcher_(dispatcher),
      name_(name),
      device_type_(device_type),
      stream_config_(fidl::Client(std::move(stream_config), dispatcher, this)),
      token_id_(NextTokenId()),
      ring_buffer_handler_(this) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();

  // Upon creation, automatically kick off initialization. This will complete asynchronously,
  // notifying the presence_watcher at that time by calling DeviceIsReady().
  Initialize();
}

Device::~Device() {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

// Invoked when the underlying driver disconnects its StreamConfig.
void Device::on_fidl_error(fidl::UnbindInfo info) {
  if (!info.is_peer_closed() && !info.is_user_initiated()) {
    ADR_WARN_OBJECT() << "StreamConfig disconnected:" << info;
    OnError(info.status());
  } else {
    ADR_LOG_OBJECT(kLogStreamConfigFidlResponses || kLogDeviceState || kLogObjectLifetimes)
        << "StreamConfig disconnected:" << info.FormatDescription();
  }

  OnRemoval();
}

// Invoked when the underlying driver disconnects its RingBuffer channel.
// We had a RingBuffer FIDL connection, so device state should be Configured/Paused/Started.
void Device::RingBufferErrorHandler::on_fidl_error(fidl::UnbindInfo info) {
  ADR_LOG_OBJECT(kLogRingBufferFidlResponses || kLogDeviceState) << "(RingBuffer)";

  if (device_->state_ == State::Error) {
    ADR_WARN_OBJECT() << "device already has an error; no device state to unwind";
    return;
  }

  // If driver encountered a significant error, then move the entire Device to Error state.
  if (!info.is_peer_closed() && !info.is_user_initiated()) {
    ADR_WARN_OBJECT() << "RingBuffer disconnected: " << info.FormatDescription();
    device_->OnError(info.status());
  } else {
    ADR_LOG_OBJECT(kLogRingBufferFidlResponses)
        << "RingBuffer disconnected:" << info.FormatDescription();
    device_->DeviceDroppedRingBuffer();
  }
}

void Device::OnRemoval() {
  ADR_LOG_OBJECT(kLogDeviceState);
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device already has an error; no device state to unwind";
    --unhealthy_count_;
  } else if (state_ != State::DeviceInitializing) {
    --initialized_count_;

    DropControl();  // Probably unneeded (Device is going away) but makes unwind "complete".
    ForEachObserver([](auto obs) { obs->DeviceIsRemoved(); });  // Our control is also an observer.
  }
  LogObjectCounts();

  // Regardless of whether device was pending / operational / unhealthy, notify the state watcher.
  if (std::shared_ptr<DevicePresenceWatcher> pw = presence_watcher_.lock(); pw) {
    pw->DeviceIsRemoved(shared_from_this());
  }
}

void Device::ForEachObserver(fit::function<void(std::shared_ptr<ObserverNotify>)> action) {
  ADR_LOG_OBJECT(kLogNotifyMethods);
  for (auto weak_obs = observers_.begin(); weak_obs < observers_.end(); ++weak_obs) {
    if (auto observer = weak_obs->lock(); observer) {
      action(observer);
    }
  }
}

// Unwind any operational state or configuration the device might be in, and remove this device.
void Device::OnError(zx_status_t error) {
  ADR_LOG_OBJECT(kLogDeviceState);
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device already has an error; ignoring subsequent error (" << error << ")";
    return;
  }

  FX_PLOGS(WARNING, error) << __func__;

  if (state_ != State::DeviceInitializing) {
    --initialized_count_;

    DropControl();
    ForEachObserver([](auto obs) { obs->DeviceHasError(); });
  }
  ++unhealthy_count_;
  SetError(error);
  LogObjectCounts();

  if (std::shared_ptr<DevicePresenceWatcher> pw = presence_watcher_.lock(); pw) {
    pw->DeviceHasError(shared_from_this());
  }
}

// An initialization command returned a successful response. Is initialization complete?
void Device::OnInitializationResponse() {
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device has already encountered a problem; ignoring this";
    return;
  }

  ADR_LOG_OBJECT(kLogDeviceInitializationProgress)
      << " (RECEIVED|pending)    "                                  //
      << (stream_config_properties_ ? "PROPS" : "props") << "    "  //
      << (supported_formats_ ? "FORMATS" : "formats") << "    "     //
      << (gain_state_ ? "GAIN" : "gain") << "      "                //
      << (plug_state_ ? "PLUG" : "plug") << "     "                 //
      << (health_state_ ? "HEALTH" : "health");

  if (state_ != State::DeviceInitializing) {
    ADR_WARN_OBJECT() << "unexpected device initialization response when not Initializing";
  }

  if (stream_config_properties_ && supported_formats_ && gain_state_ && plug_state_ &&
      health_state_) {
    ++initialized_count_;
    SetDeviceInfo();
    SetState(State::DeviceInitialized);
    LogObjectCounts();

    if (std::shared_ptr<DevicePresenceWatcher> pw = presence_watcher_.lock(); pw) {
      pw->DeviceIsReady(shared_from_this());
    }
  }
}

bool Device::SetControl(std::shared_ptr<ControlNotify> control_notify) {
  ADR_LOG_OBJECT(kLogDeviceState || kLogNotifyMethods);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device has an error; cannot set control";
    return false;
  }

  if (GetControlNotify()) {
    ADR_WARN_OBJECT() << "already controlled";
    return false;
  }

  if (state_ != State::DeviceInitialized) {
    ADR_WARN_OBJECT() << "wrong state for this call (" << state_ << ")";
    return false;
  }

  control_notify_ = control_notify;
  AddObserver(std::move(control_notify));

  LogObjectCounts();
  return true;
}

bool Device::DropControl() {
  ADR_LOG_OBJECT(kLogDeviceMethods || kLogNotifyMethods);
  FX_CHECK(state_ != State::DeviceInitializing);

  auto control_notify = GetControlNotify();
  if (!control_notify) {
    ADR_LOG_OBJECT(kLogNotifyMethods) << "already not controlled";
    return false;
  }

  control_notify_ = std::nullopt;
  // We don't remove our ControlNotify from the observer list: we wait for it to self-invalidate.

  SetState(State::DeviceInitialized);
  return true;
}

bool Device::AddObserver(std::shared_ptr<ObserverNotify> observer_to_add) {
  ADR_LOG_OBJECT(kLogDeviceMethods || kLogNotifyMethods) << " (" << observer_to_add << ")";

  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    FX_LOGS(WARNING) << "Device(" << this << ")::" << __func__ << ": unhealthy, cannot be observed";
    return false;
  }
  for (auto weak_obs = observers_.begin(); weak_obs < observers_.end(); ++weak_obs) {
    if (auto observer = weak_obs->lock(); observer) {
      if (observer == observer_to_add) {
        FX_LOGS(WARNING) << "Device(" << this << ")::AddObserver: observer cannot be re-added";
        return false;
      }
    }
  }
  observers_.push_back(observer_to_add);

  observer_to_add->GainStateChanged({{
      .gain_db = *gain_state_->gain_db(),
      .muted = gain_state_->muted().value_or(false),
      .agc_enabled = gain_state_->agc_enabled().value_or(false),
  }});
  observer_to_add->PlugStateChanged(*plug_state_->plugged()
                                        ? fuchsia_audio_device::PlugState::kPlugged
                                        : fuchsia_audio_device::PlugState::kUnplugged,
                                    zx::time(*plug_state_->plug_state_time()));

  LogObjectCounts();

  SetState(State::DeviceInitialized);
  return true;
}

// The Device dropped the driver RingBuffer FIDL. Notify any clients.
void Device::DeviceDroppedRingBuffer() {
  ADR_LOG_OBJECT(kLogDeviceState);

  // This is distinct from DropRingBuffer in case we must notify our RingBuffer (via our Control).
  // We do so if we have 1) a Control and 2) a driver_format_ (thus a client-configured RingBuffer).
  if (auto notify = GetControlNotify(); notify && driver_format_) {
    notify->DeviceDroppedRingBuffer();
  }
  DropRingBuffer();
}

// Whether client- or device-originated, reset any state associated with an active RingBuffer.
void Device::DropRingBuffer() {
  ADR_LOG_OBJECT(kLogDeviceState);

  // If we've already cleaned out any state with the underlying driver RingBuffer, then we're done.
  if (!ring_buffer_client_.is_valid()) {
    return;
  }

  if (state_ != State::Error) {
    SetState(State::DeviceInitialized);
  }

  start_time_ = std::nullopt;  // We are not started.

  delay_info_ = std::nullopt;  // We are not paused.
  num_ring_buffer_frames_ = std::nullopt;
  ring_buffer_properties_ = std::nullopt;

  driver_format_ = std::nullopt;  // We are not configured.

  // Clear our FIDL connection to the driver RingBuffer.
  ring_buffer_client_ = fidl::Client<fuchsia_hardware_audio::RingBuffer>();
}

void Device::SetError(zx_status_t error) {
  ADR_LOG_OBJECT(kLogDeviceState) << ": " << error;
  state_ = State::Error;
}

void Device::SetState(State state) {
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device already has an error; ignoring this";
    return;
  }

  ADR_LOG_OBJECT(kLogDeviceState) << state;
  state_ = state;
}

void Device::Initialize() {
  ADR_LOG_OBJECT(kLogDeviceMethods);

  RetrieveStreamProperties();
  RetrieveSupportedFormats();
  RetrieveGainState();
  RetrievePlugState();
  RetrieveHealthState();
}

// This method also sets OnError, so it is not just for logging.
// Use this when the Result might contain a domain_error or framework_error.
template <typename ResultT>
bool Device::LogResultError(const ResultT& result, const char* debug_context) {
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device already has an error; ignoring this";
    return true;
  }
  if (!result.is_ok()) {
    if (result.error_value().is_framework_error()) {
      if (result.error_value().framework_error().is_canceled() ||
          result.error_value().framework_error().is_peer_closed()) {
        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
            << debug_context << ": will take no action on "
            << result.error_value().FormatDescription();
      } else {
        FX_LOGS(ERROR) << debug_context << " failed: " << result.error_value().FormatDescription()
                       << ")";
        OnError(result.error_value().framework_error().status());
      }
    } else {
      OnError(ZX_ERR_INTERNAL);
    }
    return true;
  }
  return false;
}

// This method also sets OnError, so it is not just for logging.
// Use this when the Result error can only be a framework_error.
template <typename ResultT>
bool Device::LogResultFrameworkError(const ResultT& result, const char* debug_context) {
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device already has an error; ignoring this";
    return true;
  }
  if (!result.is_ok()) {
    if (result.error_value().is_canceled() || result.error_value().is_peer_closed()) {
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

void Device::RetrieveStreamProperties() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  // TODO(fxbug.dev/113429): handle command timeouts

  stream_config_->GetProperties().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetProperties>& result) {
        if (LogResultFrameworkError(result, "GetProperties response")) {
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

void Device::RetrieveSupportedFormats() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  RetrieveFormats(
      [this](std::vector<fuchsia_hardware_audio::SupportedFormats> supported_format_sets) {
        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses);
        supported_formats_ = std::vector<fuchsia_hardware_audio::SupportedFormats>();
        for (const auto& supported_format_set : supported_format_sets) {
          supported_formats_->emplace_back(supported_format_set);
        }
        OnInitializationResponse();
      });
}

void Device::RetrieveGainState() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  if (!gain_state_) {
    // TODO(fxbug.dev/113429): handle command timeouts (but not on subsequent watches)
  }

  stream_config_->WatchGainState().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::WatchGainState>& result) {
        if (LogResultFrameworkError(result, "GainState response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchGainState response";
        auto status = ValidateGainState(result->gain_state());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        auto old_gain_state = gain_state_;
        gain_state_ = result->gain_state();
        gain_state_->muted() = gain_state_->muted().value_or(false);
        gain_state_->agc_enabled() = gain_state_->agc_enabled().value_or(false);

        if (!old_gain_state) {
          ADR_LOG_CLASS(kLogStreamConfigFidlResponses) << "WatchGainState received initial value";
          OnInitializationResponse();
        } else {
          ADR_LOG_CLASS(kLogStreamConfigFidlResponses) << "WatchGainState received update";
          ForEachObserver([gain_state = *gain_state_](auto obs) {
            obs->GainStateChanged({{
                .gain_db = *gain_state.gain_db(),
                .muted = gain_state.muted().value_or(false),
                .agc_enabled = gain_state.agc_enabled().value_or(false),
            }});
          });
        }
        // Kick off the next watch.
        RetrieveGainState();
      });
}

void Device::RetrievePlugState() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  if (!plug_state_) {
    // TODO(fxbug.dev/113429): handle command timeouts (but not on subsequent watches)
  }

  stream_config_->WatchPlugState().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::WatchPlugState>& result) {
        if (LogResultFrameworkError(result, "PlugState response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchPlugState response";
        auto status = ValidatePlugState(result->plug_state(), stream_config_properties_);
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        auto old_plug_state = plug_state_;
        plug_state_ = result->plug_state();

        if (!plug_state_) {
          ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchPlugState received initial value";
          OnInitializationResponse();
        } else {
          ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchPlugState received update";
          ForEachObserver([plug_state = *plug_state_](auto obs) {
            obs->PlugStateChanged(plug_state.plugged().value_or(true)
                                      ? fuchsia_audio_device::PlugState::kPlugged
                                      : fuchsia_audio_device::PlugState::kUnplugged,
                                  zx::time(*plug_state.plug_state_time()));
          });
        }
        // Kick off the next watch.
        RetrievePlugState();
      });
}

// TODO(fxbug.dev/117199): Decide when we proactively call GetHealthState, if at all.
void Device::RetrieveHealthState() {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }

  // TODO(fxbug.dev/113429): handle command timeouts

  stream_config_->GetHealthState().Then(
      [this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetHealthState>& result) {
        if (LogResultFrameworkError(result, "HealthState response")) {
          return;
        }

        auto old_health_state = health_state_;

        // An empty health state is permitted; it still indicates that the driver is responsive.
        health_state_ = result->state().healthy().value_or(true);
        // ...but if the driver actually self-reported as unhealthy, this is a problem.
        if (!*health_state_) {
          FX_LOGS(WARNING) << "RetrieveHealthState response: .healthy is FALSE (unhealthy)";
          OnError(ZX_ERR_IO);
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "RetrieveHealthState response: healthy";
        if (!old_health_state) {
          OnInitializationResponse();
        }
      });
}

void Device::RetrieveFormats(
    fit::callback<void(std::vector<fuchsia_hardware_audio::SupportedFormats>)>
        supported_formats_callback) {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    return;
  }
  // TODO(fxbug.dev/113429): handle command timeouts

  stream_config_->GetSupportedFormats().Then(
      [this, callback = std::move(supported_formats_callback)](
          fidl::Result<fuchsia_hardware_audio::StreamConfig::GetSupportedFormats>& result) mutable {
        if (LogResultFrameworkError(result, "SupportedFormats response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
            << "StreamConfig/GetSupportedFormats: success";
        auto status = ValidateSupportedFormats(result->supported_formats());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        callback(result->supported_formats());
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

      .supported_formats = TranslateFormatSets(*supported_formats_),
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
zx::result<zx::clock> Device::GetReadOnlyClock() const {
  ADR_LOG_OBJECT(kLogDeviceMethods);

  auto dupe_clock = device_clock_->DuplicateZxClockReadOnly();
  if (!dupe_clock) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  return zx::ok(std::move(*dupe_clock));
}

// Determine the full fuchsia_hardware_audio::Format needed for ConnectRingBufferFidl.
// This method expects that the required fields are present.
std::optional<fuchsia_hardware_audio::Format> Device::SupportedDriverFormatForClientFormat(
    // TODO(fxbug.dev/117829): Consider using media_audio::Format internally.
    const fuchsia_audio::Format& client_format) {
  fuchsia_hardware_audio::SampleFormat driver_sample_format;
  uint8_t bytes_per_sample, max_valid_bits;
  auto client_sample_type = *client_format.sample_type();
  auto channel_count = *client_format.channel_count();
  auto frame_rate = *client_format.frames_per_second();

  switch (client_sample_type) {
    case fuchsia_audio::SampleType::kUint8:
      driver_sample_format = fuchsia_hardware_audio::SampleFormat::kPcmUnsigned;
      max_valid_bits = 8;
      bytes_per_sample = 1;
      break;
    case fuchsia_audio::SampleType::kInt16:
      driver_sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned;
      max_valid_bits = 16;
      bytes_per_sample = 2;
      break;
    case fuchsia_audio::SampleType::kInt32:
      driver_sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned;
      max_valid_bits = 32;
      bytes_per_sample = 4;
      break;
    case fuchsia_audio::SampleType::kFloat32:
      driver_sample_format = fuchsia_hardware_audio::SampleFormat::kPcmFloat;
      max_valid_bits = 32;
      bytes_per_sample = 4;
      break;
    case fuchsia_audio::SampleType::kFloat64:
      driver_sample_format = fuchsia_hardware_audio::SampleFormat::kPcmFloat;
      max_valid_bits = 64;
      bytes_per_sample = 8;
      break;
    default:
      FX_CHECK(false) << "Unhandled fuchsia_audio::SampleType: "
                      << static_cast<uint32_t>(client_sample_type);
  }

  // If format/bytes/rate/channels all match, save the highest valid_bits within our limit.
  uint8_t best_valid_bits = 0;
  for (const auto& supported_formats : *supported_formats_) {
    const auto format_set = *supported_formats.pcm_supported_formats();
    if (std::count_if(
            format_set.sample_formats()->begin(), format_set.sample_formats()->end(),
            [driver_sample_format](const auto& f) { return f == driver_sample_format; }) &&
        std::count_if(format_set.bytes_per_sample()->begin(), format_set.bytes_per_sample()->end(),
                      [bytes_per_sample](const auto& bs) { return bs == bytes_per_sample; }) &&
        std::count_if(format_set.frame_rates()->begin(), format_set.frame_rates()->end(),
                      [frame_rate](const auto& fr) { return fr == frame_rate; }) &&
        std::count_if(format_set.channel_sets()->begin(), format_set.channel_sets()->end(),
                      [channel_count](const fuchsia_hardware_audio::ChannelSet& cs) {
                        return cs.attributes()->size() == channel_count;
                      })) {
      std::for_each(format_set.valid_bits_per_sample()->begin(),
                    format_set.valid_bits_per_sample()->end(),
                    [max_valid_bits, &best_valid_bits](uint8_t v_bits) {
                      if (v_bits <= max_valid_bits) {
                        best_valid_bits = std::max(best_valid_bits, v_bits);
                      }
                    });
    }
  }

  if (!best_valid_bits) {
    FX_LOGS(WARNING) << __func__ << ": no intersection for client format: "
                     << static_cast<uint16_t>(channel_count) << "-chan " << frame_rate << "hz "
                     << client_sample_type;
    return {};
  }

  ADR_LOG_OBJECT(kLogRingBufferFidlResponseValues)
      << "successful match for client format: " << channel_count << "-chan " << frame_rate << "hz "
      << client_sample_type << " (valid_bits " << static_cast<uint16_t>(best_valid_bits) << ")";

  return fuchsia_hardware_audio::Format{{
      fuchsia_hardware_audio::PcmFormat{{
          .number_of_channels = static_cast<uint8_t>(channel_count),
          .sample_format = driver_sample_format,
          .bytes_per_sample = bytes_per_sample,
          .valid_bits_per_sample = best_valid_bits,
          .frame_rate = frame_rate,
      }},
  }};
}

void Device::GetCurrentlyPermittedFormats(
    fit::callback<void(std::vector<fuchsia_audio_device::PcmFormatSet>)>
        permitted_formats_callback) {
  ADR_LOG_OBJECT(kLogRingBufferMethods);

  RetrieveFormats(
      [this, callback = std::move(permitted_formats_callback)](
          std::vector<fuchsia_hardware_audio::SupportedFormats> permitted_format_sets) mutable {
        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses);
        permitted_formats_ = TranslateFormatSets(permitted_format_sets);
        callback(permitted_formats_);
      });
}

bool Device::SetGain(fuchsia_hardware_audio::GainState& gain_state) {
  ADR_LOG_OBJECT(kLogStreamConfigFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "Device has previous error; cannot set gain";
    return false;
  }
  if (!GetControlNotify()) {
    ADR_WARN_OBJECT() << "Device must be allocated before this method can be called";
    return false;
  }

  auto status = stream_config_->SetGain(std::move(gain_state));
  if (status.is_error()) {
    if (status.error_value().is_canceled()) {
      // These indicate that we are already shutting down, so they aren't error conditions.
      ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
          << "SetGain response will take no action on error "
          << status.error_value().FormatDescription();

      return false;
    }

    FX_PLOGS(ERROR, status.error_value().status()) << __func__ << " returned error:";
    OnError(status.error_value().status());
    return false;
  }

  ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << " is_ok";

  // We don't notify anyone - we wait for the driver to notify us via WatchGainState.
  return true;
}

// If the optional<weak_ptr> is set AND the weak_ptr can be locked to its shared_ptr, then the
// resulting shared_ptr is returned. Otherwise, nullptr is returned, after first resetting the
// optional `control_notify` if it is set but the weak_ptr is no longer valid.
std::shared_ptr<ControlNotify> Device::GetControlNotify() {
  if (!control_notify_) {
    return nullptr;
  }

  auto sh_ptr_control = control_notify_->lock();
  if (!sh_ptr_control) {
    control_notify_ = std::nullopt;
    LogObjectCounts();
  }

  return sh_ptr_control;
}

bool Device::CreateRingBuffer(const fuchsia_hardware_audio::Format& format,
                              uint32_t requested_ring_buffer_bytes,
                              fit::callback<void(RingBufferInfo)> create_ring_buffer_callback) {
  ADR_LOG_OBJECT(kLogRingBufferMethods);
  if (!ConnectRingBufferFidl(format)) {
    return false;
  }

  requested_ring_buffer_bytes_ = requested_ring_buffer_bytes;
  create_ring_buffer_callback_ = std::move(create_ring_buffer_callback);

  RetrieveRingBufferProperties();
  RetrieveDelayInfo();

  return true;
}

bool Device::ConnectRingBufferFidl(fuchsia_hardware_audio::Format driver_format) {
  ADR_LOG_OBJECT(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  auto status = ValidateRingBufferFormat(driver_format);
  if (status != ZX_OK) {
    OnError(status);
    return false;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
  if (!endpoints.is_ok()) {
    FX_PLOGS(ERROR, endpoints.status_value())
        << "CreateEndpoints<fuchsia_hardware_audio::RingBuffer> failed";
    OnError(endpoints.status_value());
    return false;
  }

  auto result = stream_config_->CreateRingBuffer({driver_format, std::move(endpoints->server)});
  if (!result.is_ok()) {
    FX_PLOGS(ERROR, result.error_value().status()) << "StreamConfig/CreateRingBuffer failed";
    OnError(result.error_value().status());
    return false;
  }

  ring_buffer_client_.Bind(
      fidl::ClientEnd<fuchsia_hardware_audio::RingBuffer>(std::move(endpoints->client)),
      dispatcher_, &ring_buffer_handler_);

  auto bytes_per_sample = driver_format.pcm_format()->bytes_per_sample();
  auto sample_format = driver_format.pcm_format()->sample_format();
  fuchsia_audio::SampleType sample_type;
  if (bytes_per_sample == 1) {
    if (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmUnsigned) {
      sample_type = fuchsia_audio::SampleType::kUint8;
    }
  } else if (bytes_per_sample == 2) {
    if (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmSigned) {
      sample_type = fuchsia_audio::SampleType::kInt16;
    }
  } else if (bytes_per_sample == 4) {
    if (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmSigned) {
      sample_type = fuchsia_audio::SampleType::kInt32;
    } else if (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmFloat) {
      sample_type = fuchsia_audio::SampleType::kFloat32;
    }
  } else if (bytes_per_sample == 8) {
    if (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmFloat) {
      sample_type = fuchsia_audio::SampleType::kFloat64;
    }
  }
  driver_format_ = driver_format;  // This contains valid_bits_per_sample.
  vmo_format_ = {{
      .sample_type = sample_type,
      .channel_count = driver_format.pcm_format()->number_of_channels(),
      .frames_per_second = driver_format.pcm_format()->frame_rate(),
      // TODO(fxbug.dev/87650): handle .channel_layout, when communicated from driver.
  }};

  active_channels_bitmask_ = (1 << *vmo_format_.channel_count()) - 1;
  SetActiveChannels(active_channels_bitmask_, [this](zx::result<zx::time> result) {
    if (result.is_ok()) {
      ADR_LOG_CLASS(kLogRingBufferFidlResponses) << "RingBuffer/SetActiveChannels IS supported";
      return;
    }
    if (result.status_value() == ZX_ERR_NOT_SUPPORTED) {
      ADR_LOG_CLASS(kLogRingBufferFidlResponses) << "RingBuffer/SetActiveChannels IS NOT supported";
      return;
    }
    ADR_WARN_OBJECT() << "RingBuffer/SetActiveChannels returned error: " << result.status_string();
    OnError(result.status_value());
  });
  SetState(State::CreatingRingBuffer);

  return true;
}

void Device::RetrieveRingBufferProperties() {
  ADR_LOG_OBJECT(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  ring_buffer_client_->GetProperties().Then(
      [this](fidl::Result<fuchsia_hardware_audio::RingBuffer::GetProperties>& result) {
        if (LogResultFrameworkError(result, "RingBuffer/GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/GetProperties: success";
        auto status = ValidateRingBufferProperties(result->properties());
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "RingBuffer/GetProperties error: "
                         << result.error_value().FormatDescription();
          OnError(status);
          return;
        }

        ring_buffer_properties_ = result->properties();
        CheckForRingBufferReady();
      });
}

void Device::RetrieveDelayInfo() {
  ADR_LOG_OBJECT(kLogRingBufferMethods || kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.is_valid());

  ring_buffer_client_->WatchDelayInfo().Then(
      [this](fidl::Result<fuchsia_hardware_audio::RingBuffer::WatchDelayInfo>& result) {
        if (LogResultFrameworkError(result, "RingBuffer/WatchDelayInfo response")) {
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/WatchDelayInfo: success";

        auto status = ValidateDelayInfo(result->delay_info(), ring_buffer_properties_,
                                        *driver_format_->pcm_format());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        delay_info_ = result->delay_info();
        // If requested_ring_buffer_bytes_ is already set, but num_ring_buffer_frames_ isn't, then
        // we're getting delay info as part of creating a ring buffer. Otherwise,
        // requested_ring_buffer_bytes_ must be set separately before calling GetVmo.
        if (requested_ring_buffer_bytes_ && !num_ring_buffer_frames_) {
          // Needed, to set requested_ring_buffer_frames_ before calling GetVmo.
          CalculateRequiredRingBufferSizes();

          FX_CHECK(device_info_->clock_domain());
          const auto clock_position_notifications_per_ring =
              *device_info_->clock_domain() == fuchsia_hardware_audio::kClockDomainMonotonic ? 0
                                                                                             : 2;
          GetVmo(static_cast<uint32_t>(requested_ring_buffer_frames_),
                 clock_position_notifications_per_ring);
        }

        // Notify our controlling entity, if we have one.
        if (auto notify = GetControlNotify(); notify) {
          notify->DelayInfoChanged({{
              .internal_delay = delay_info_->internal_delay(),
              .external_delay = delay_info_->external_delay(),
          }});
        }
        RetrieveDelayInfo();
      });
}

void Device::GetVmo(uint32_t min_frames, uint32_t position_notifications_per_ring) {
  ADR_LOG_OBJECT(kLogRingBufferMethods || kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.is_valid());
  FX_CHECK(driver_format_);

  ring_buffer_client_
      ->GetVmo({{.min_frames = min_frames,
                 .clock_recovery_notifications_per_ring = position_notifications_per_ring}})
      .Then([this](fidl::Result<fuchsia_hardware_audio::RingBuffer::GetVmo>& result) {
        if (LogResultError(result, "RingBuffer/GetVmo response")) {
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/GetVmo: success";

        auto status =
            ValidateRingBufferVmo(result->ring_buffer(), result->num_frames(), *driver_format_);

        if (status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "Error in RingBuffer/GetVmo response";
          OnError(status);
          return;
        }
        ring_buffer_vmo_ = std::move(result->ring_buffer());
        num_ring_buffer_frames_ = result->num_frames();
        CheckForRingBufferReady();
      });
}

// RingBuffer FIDL successful-response handlers.
void Device::CheckForRingBufferReady() {
  ADR_LOG_OBJECT(kLogRingBufferFidlResponses);
  if (state_ == State::Error) {
    ADR_WARN_OBJECT() << "device has previous error; cannot CheckForRingBufferReady";
    return;
  }

  // Check whether we are tearing down, or conversely have already set up the ring buffer.
  if (state_ != State::CreatingRingBuffer) {
    return;
  }

  // We're creating the ring buffer but don't have all our prerequisites yet.
  if (!ring_buffer_properties_ || !delay_info_ || !num_ring_buffer_frames_) {
    return;
  }

  auto ref_clock = GetReadOnlyClock();
  if (!ref_clock.is_ok()) {
    ADR_WARN_OBJECT() << "reference clock is not ok";
    return;
  }

  SetState(State::RingBufferStopped);

  FX_CHECK(create_ring_buffer_callback_);
  create_ring_buffer_callback_({
      .ring_buffer = fuchsia_audio::RingBuffer{{
          .buffer = fuchsia_mem::Buffer{{
              .vmo = std::move(ring_buffer_vmo_),
              .size = *num_ring_buffer_frames_ * bytes_per_frame_,
          }},
          .format = vmo_format_,
          .producer_bytes = ring_buffer_producer_bytes_,
          .consumer_bytes = ring_buffer_consumer_bytes_,
          .reference_clock = std::move(*ref_clock),
          .reference_clock_domain = *device_info_->clock_domain(),
      }},
      .properties = fuchsia_audio_device::RingBufferProperties{{
          .valid_bits_per_sample = valid_bits_per_sample(),
          .turn_on_delay = ring_buffer_properties_->turn_on_delay().value_or(0),
      }},
  });
  create_ring_buffer_callback_ = nullptr;
}

void Device::SetActiveChannels(
    uint64_t channel_bitmask,
    fit::callback<void(zx::result<zx::time>)> set_active_channels_callback) {
  ADR_LOG_OBJECT(kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.is_valid());

  // If we already know this device doesn't support SetActiveChannels, do nothing.
  if (!supports_set_active_channels_.value_or(true)) {
    return;
  }
  ring_buffer_client_->SetActiveChannels({{.active_channels_bitmask = channel_bitmask}})
      .Then(
          [this, channel_bitmask, callback = std::move(set_active_channels_callback)](
              fidl::Result<fuchsia_hardware_audio::RingBuffer::SetActiveChannels>& result) mutable {
            if (result.is_error() && result.error_value().is_domain_error() &&
                result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED) {
              ADR_LOG_OBJECT(kLogRingBufferFidlResponses)
                  << "RingBuffer/SetActiveChannels: device does not support this method";
              supports_set_active_channels_ = false;
              callback(zx::error(ZX_ERR_NOT_SUPPORTED));
              return;
            }
            if (LogResultError(result, "RingBuffer/SetActiveChannels response")) {
              supports_set_active_channels_ = false;
              callback(zx::error(ZX_ERR_INTERNAL));
              return;
            }

            ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/SetActiveChannels: success";

            supports_set_active_channels_ = true;
            active_channels_bitmask_ = channel_bitmask;
            active_channels_set_time_ = zx::time(result->set_time());
            callback(zx::ok(*active_channels_set_time_));
            LogActiveChannels(active_channels_bitmask_, *active_channels_set_time_);
          });
}

void Device::StartRingBuffer(fit::callback<void(zx::result<zx::time>)> start_callback) {
  ADR_LOG_OBJECT(kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.is_valid());

  ring_buffer_client_->Start().Then(
      [this, callback = std::move(start_callback)](
          fidl::Result<fuchsia_hardware_audio::RingBuffer::Start>& result) mutable {
        if (LogResultFrameworkError(result, "RingBuffer/Start response")) {
          callback(zx::error(ZX_ERR_INTERNAL));
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/Start: success";

        start_time_ = zx::time(result->start_time());
        callback(zx::ok(*start_time_));
        SetState(State::RingBufferStarted);
      });
}

void Device::StopRingBuffer(fit::callback<void(zx_status_t)> stop_callback) {
  ADR_LOG_OBJECT(kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.is_valid());

  ring_buffer_client_->Stop().Then(
      [this, callback = std::move(stop_callback)](
          fidl::Result<fuchsia_hardware_audio::RingBuffer::Stop>& result) mutable {
        if (LogResultFrameworkError(result, "RingBuffer/Stop response")) {
          callback(ZX_ERR_INTERNAL);
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/Stop: success";

        start_time_ = std::nullopt;
        callback(ZX_OK);
        SetState(State::RingBufferStopped);
      });
}

// Uses the VMO format and ring buffer properties, to set bytes_per_frame_,
// requested_ring_buffer_frames_, ring_buffer_consumer_bytes_ and ring_buffer_producer_bytes_.
void Device::CalculateRequiredRingBufferSizes() {
  ADR_LOG_OBJECT(kLogRingBufferMethods);

  FX_CHECK(vmo_format_.channel_count());
  FX_CHECK(vmo_format_.sample_type());
  FX_CHECK(vmo_format_.frames_per_second());
  FX_CHECK(ring_buffer_properties_);
  FX_CHECK(ring_buffer_properties_->driver_transfer_bytes());
  FX_CHECK(requested_ring_buffer_bytes_);
  FX_CHECK(*requested_ring_buffer_bytes_ > 0);

  switch (*vmo_format_.sample_type()) {
    case fuchsia_audio::SampleType::kUint8:
      bytes_per_frame_ = 1;
      break;
    case fuchsia_audio::SampleType::kInt16:
      bytes_per_frame_ = 2;
      break;
    case fuchsia_audio::SampleType::kInt32:
    case fuchsia_audio::SampleType::kFloat32:
      bytes_per_frame_ = 4;
      break;
    case fuchsia_audio::SampleType::kFloat64:
      bytes_per_frame_ = 8;
      break;
    default:
      FX_LOGS(FATAL) << __func__ << ": unknown fuchsia_audio::SampleType";
      __UNREACHABLE;
  }
  bytes_per_frame_ *= *vmo_format_.channel_count();

  requested_ring_buffer_frames_ = media::TimelineRate{1, bytes_per_frame_}.Scale(
      *requested_ring_buffer_bytes_, media::TimelineRate::RoundingMode::Ceiling);

  // We don't include driver transfer size in our VMO size request (requested_ring_buffer_frames_)
  // ... but we do communicate it in our description of ring buffer producer/consumer "zones".
  uint64_t driver_bytes = *ring_buffer_properties_->driver_transfer_bytes() + bytes_per_frame_ - 1;
  driver_bytes -= (driver_bytes % bytes_per_frame_);

  if (*device_info_->device_type() == fuchsia_audio_device::DeviceType::kOutput) {
    ring_buffer_consumer_bytes_ = driver_bytes;
    ring_buffer_producer_bytes_ = requested_ring_buffer_frames_ * bytes_per_frame_;
  } else {
    ring_buffer_producer_bytes_ = driver_bytes;
    ring_buffer_consumer_bytes_ = requested_ring_buffer_frames_ * bytes_per_frame_;
  }

  // TODO(fxbug.dev/117826): validate this case; we don't surface this error to the caller.
  if (requested_ring_buffer_frames_ > std::numeric_limits<uint32_t>::max()) {
    ADR_WARN_OBJECT() << "requested_ring_buffer_frames_ cannot exceed uint32_t::max()";
    requested_ring_buffer_frames_ = std::numeric_limits<uint32_t>::max();
  }
}

// TODO(fxbug.dev/117827): implement this, via hanging RingBuffer/WatchClockRecoveryPositionInfo.
void Device::RecoverDeviceClockFromPositionInfo() { ADR_LOG_OBJECT(kLogRingBufferMethods); }

// TODO(fxbug.dev/117827): implement this.
void Device::StopDeviceClockRecovery() { ADR_LOG_OBJECT(kLogRingBufferMethods); }

}  // namespace media_audio

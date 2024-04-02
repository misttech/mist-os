// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/markers.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/channel.h>
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
#include "src/media/audio/services/device_registry/signal_processing_utils.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

TokenId NextTokenId() {
  static TokenId token_id = 0;
  return token_id++;
}

// static
std::shared_ptr<Device> Device::Create(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
                                       async_dispatcher_t* dispatcher, std::string_view name,
                                       fuchsia_audio_device::DeviceType device_type,
                                       fuchsia_audio_device::DriverClient driver_client) {
  ADR_LOG_STATIC(kLogObjectLifetimes);

  // The constructor is private, forcing clients to use Device::Create().
  class MakePublicCtor : public Device {
   public:
    MakePublicCtor(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
                   async_dispatcher_t* dispatcher, std::string_view name,
                   fuchsia_audio_device::DeviceType device_type,
                   fuchsia_audio_device::DriverClient driver_client)
        : Device(std::move(presence_watcher), dispatcher, name, device_type,
                 std::move(driver_client)) {}
  };

  return std::make_shared<MakePublicCtor>(presence_watcher, dispatcher, name, device_type,
                                          std::move(driver_client));
}

// Device notifies presence_watcher when it is available (DeviceInitialized), unhealthy (Error) or
// removed. The dispatcher member is needed for Device to create client connections to protocols
// such as fuchsia.hardware.audio.signalprocessing.Reader or fuchsia.hardware.audio.RingBuffer.
Device::Device(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
               async_dispatcher_t* dispatcher, std::string_view name,
               fuchsia_audio_device::DeviceType device_type,
               fuchsia_audio_device::DriverClient driver_client)
    : presence_watcher_(std::move(presence_watcher)),
      dispatcher_(dispatcher),
      name_(name.substr(0, name.find('\0'))),
      device_type_(device_type),
      driver_client_(std::move(driver_client)),
      token_id_(NextTokenId()) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();

  switch (device_type_) {
    case fuchsia_audio_device::DeviceType::kCodec:
      codec_handler_ = {this, "Codec"};
      codec_client_ = {driver_client_.codec().take().value(), dispatcher, &codec_handler_};
      break;
    case fuchsia_audio_device::DeviceType::kInput:
    case fuchsia_audio_device::DeviceType::kOutput:
      stream_config_handler_ = {this, "StreamConfig"};
      stream_config_client_ = {driver_client_.stream_config().take().value(), dispatcher,
                               &stream_config_handler_};
      break;
    case fuchsia_audio_device::DeviceType::kComposite:
      ADR_WARN_METHOD() << "Composite device type is not yet implemented";
      OnError(ZX_ERR_WRONG_TYPE);
      return;
    case fuchsia_audio_device::DeviceType::kDai:
      ADR_WARN_METHOD() << "Dai device type is not yet implemented";
      OnError(ZX_ERR_WRONG_TYPE);
      return;
    default:
      ADR_WARN_METHOD() << "Unknown DeviceType" << device_type_;
      // Set device to error state
      OnError(ZX_ERR_WRONG_TYPE);
      return;
  }

  // Upon creation, automatically kick off initialization. This will complete asynchronously,
  // notifying the presence_watcher at that time by calling DeviceIsReady().
  Initialize();
}

Device::~Device() {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  --count_;

  LogObjectCounts();
}

// Invoked when a RingBuffer channel drops. Device state previously was Configured/Paused/Started.
template <>
void Device::FidlErrorHandler<fuchsia_hardware_audio::RingBuffer>::on_fidl_error(
    fidl::UnbindInfo info) {
  ADR_LOG_METHOD(kLogRingBufferFidlResponses || kLogDeviceState) << "(RingBuffer)";
  if (device_->state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; no device state to unwind";
  } else if (info.is_peer_closed() || info.is_user_initiated()) {
    ADR_LOG_METHOD(kLogRingBufferFidlResponses) << name_ << " disconnected: " << info;
    device_->DeviceDroppedRingBuffer();
  } else {
    ADR_WARN_METHOD() << name_ << " disconnected: " << info;
    device_->OnError(info.status());
  }
  device_->ring_buffer_client_.reset();
}

// Invoked when a SignalProcessing channel drops. This can occur during device initialization.
template <>
void Device::FidlErrorHandler<fuchsia_hardware_audio_signalprocessing::SignalProcessing>::
    on_fidl_error(fidl::UnbindInfo info) {
  ADR_LOG_METHOD(kLogRingBufferFidlResponses || kLogDeviceState) << "(SignalProcessing)";
  // If a device already encountered some other error, disconnects like this are not unexpected.
  if (device_->state_ == State::Error) {
    ADR_WARN_METHOD() << "(signalprocessing) device already has error; no state to unwind";
    return;
  }
  // Check whether this occurred as a normal part of checking for signalprocessing support.
  if (info.is_peer_closed() && info.status() == ZX_ERR_NOT_SUPPORTED) {
    ADR_LOG_METHOD(kLogSignalProcessingFidlResponses || kLogDeviceState)
        << name_ << ": signalprocessing not supported: " << info;
    device_->SetSignalProcessingSupported(false);
    return;
  }
  // Otherwise, we might just be unwinding this device (device-removal, or service shut-down).
  if (info.is_peer_closed() || info.is_user_initiated()) {
    ADR_LOG_METHOD(kLogSignalProcessingFidlResponses || kLogDeviceState)
        << name_ << "(signalprocessing) disconnected: " << info;
    device_->SetSignalProcessingSupported(false);
    return;
  }
  // If none of the above, this driver's FIDL connection is now in doubt. Mark it as in ERROR.
  ADR_WARN_METHOD() << name_ << "(signalprocessing) disconnected: " << info;
  device_->SetSignalProcessingSupported(false);
  device_->OnError(info.status());

  device_->sig_proc_client_.reset();
}

void Device::SetSignalProcessingSupported(bool is_supported) {
  auto first_set_of_signalprocessing_support = !supports_signalprocessing_.has_value();

  supports_signalprocessing_ = is_supported;

  // Only poke the initialization state machine the FIRST time this is called.
  if (first_set_of_signalprocessing_support) {
    OnInitializationResponse();
  }
}

// Invoked when the underlying driver disconnects.
template <typename T>
void Device::FidlErrorHandler<T>::on_fidl_error(fidl::UnbindInfo info) {
  if (!info.is_peer_closed() && !info.is_user_initiated() && !info.is_dispatcher_shutdown()) {
    ADR_WARN_METHOD() << name_ << " disconnected: " << info;
    device_->OnError(info.status());
  } else {
    ADR_LOG_METHOD(kLogCodecFidlResponses || kLogStreamConfigFidlResponses || kLogDeviceState ||
                   kLogObjectLifetimes)
        << name_ << " disconnected: " << info;
  }
  device_->OnRemoval();
}

void Device::OnRemoval() {
  ADR_LOG_METHOD(kLogDeviceState);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; no device state to unwind";
    --unhealthy_count_;
  } else if (state_ != State::DeviceInitializing) {
    --initialized_count_;

    DropControl();  // Probably unneeded (the Device is going away) but makes unwind "complete".
    ForEachObserver([](auto obs) { obs->DeviceIsRemoved(); });  // Our control is also an observer.
  }

  ring_buffer_client_.reset();
  sig_proc_client_.reset();
  codec_client_.reset();
  stream_config_client_.reset();

  LogObjectCounts();

  // Regardless of whether device was pending / operational / unhealthy, notify the state watcher.
  if (std::shared_ptr<DevicePresenceWatcher> pw = presence_watcher_.lock(); pw) {
    pw->DeviceIsRemoved(shared_from_this());
  }
}

void Device::ForEachObserver(fit::function<void(std::shared_ptr<ObserverNotify>)> action) {
  ADR_LOG_METHOD(kLogNotifyMethods);
  for (auto weak_obs = observers_.begin(); weak_obs < observers_.end(); ++weak_obs) {
    if (auto observer = weak_obs->lock(); observer) {
      action(observer);
    }
  }
}

// Unwind any operational state or configuration the device might be in, and remove this device.
void Device::OnError(zx_status_t error) {
  ADR_LOG_METHOD(kLogDeviceState);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; ignoring subsequent error (" << error << ")";
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
bool Device::IsFullyInitialized() {
  switch (device_type_) {
    case fuchsia_audio_device::DeviceType::kCodec:
      return has_codec_properties() && has_health_state() && checked_for_signalprocessing() &&
             dai_format_sets_retrieved() && has_plug_state();
    case fuchsia_audio_device::DeviceType::kInput:
    case fuchsia_audio_device::DeviceType::kOutput:
      return has_stream_config_properties() && has_health_state() &&
             checked_for_signalprocessing() && ring_buffer_format_sets_retrieved() &&
             has_gain_state() && has_plug_state();
    case fuchsia_audio_device::DeviceType::kComposite:
      ADR_WARN_METHOD() << "Don't yet support Composite";
      return false;
    case fuchsia_audio_device::DeviceType::kDai:
      ADR_WARN_METHOD() << "Don't yet support Dai";
      return false;
    default:
      ADR_WARN_METHOD() << "Invalid device_type_";
      return false;
  }
}

// An initialization command returned a successful response. Is initialization complete?
void Device::OnInitializationResponse() {
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device has already encountered a problem; ignoring this";
    return;
  }

  if (state_ != State::DeviceInitializing) {
    ADR_WARN_METHOD() << "unexpected device initialization response when not Initializing";
  }

  switch (device_type_) {
    case fuchsia_audio_device::DeviceType::kCodec:
      ADR_LOG_METHOD(kLogDeviceInitializationProgress)
          << " (RECEIVED|pending)"                                                 //
          << "   " << (has_codec_properties() ? "PROPS" : "props")                 //
          << "   " << (has_health_state() ? "HEALTH" : "health")                   //
          << "   " << (checked_for_signalprocessing() ? "SIGPROC" : "sigproc")     //
          << "   " << (dai_format_sets_retrieved() ? "DAIFORMATS" : "daiformats")  //
          << "   " << (has_plug_state() ? "PLUG" : "plug");
      break;
    case fuchsia_audio_device::DeviceType::kInput:
    case fuchsia_audio_device::DeviceType::kOutput:
      ADR_LOG_METHOD(kLogDeviceInitializationProgress)
          << " (RECEIVED|pending)"                                                         //
          << "   " << (has_stream_config_properties() ? "PROPS" : "props")                 //
          << "   " << (has_health_state() ? "HEALTH" : "health")                           //
          << "   " << (checked_for_signalprocessing() ? "SIGPROC" : "sigproc")             //
          << "   " << (ring_buffer_format_sets_retrieved() ? "RB_FORMATS" : "rb_formats")  //
          << "   " << (has_plug_state() ? "PLUG" : "plug")                                 //
          << "   " << (has_gain_state() ? "GAIN" : "gain");
      break;
    case fuchsia_audio_device::DeviceType::kComposite:
    case fuchsia_audio_device::DeviceType::kDai:
    default:
      ADR_WARN_METHOD() << "Invalid device_type_";
      return;
  }

  if (IsFullyInitialized()) {
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
  ADR_LOG_METHOD(kLogDeviceState || kLogNotifyMethods);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device has an error; cannot set control";
    return false;
  }

  if (GetControlNotify()) {
    ADR_WARN_METHOD() << "already controlled";
    return false;
  }

  if (state_ != State::DeviceInitialized) {
    ADR_WARN_METHOD() << "wrong state for this call (" << state_ << ")";
    return false;
  }

  control_notify_ = control_notify;
  AddObserver(std::move(control_notify));

  // For this new control, "catch it up" on the current state.
  if (auto notify = GetControlNotify(); notify) {
    // DaiFormat(s)
    if (device_type_ == fuchsia_audio_device::DeviceType::kCodec) {
      if (codec_format_) {
        notify->DaiFormatChanged(codec_format_->dai_format, codec_format_->codec_format_info);
      } else {
        notify->DaiFormatChanged(std::nullopt, std::nullopt);
      }
      // Codec start state
      codec_start_state_.started ? notify->CodecStarted(codec_start_state_.start_stop_time)
                                 : notify->CodecStopped(codec_start_state_.start_stop_time);
    }
  }

  LogObjectCounts();
  return true;
}

bool Device::DropControl() {
  ADR_LOG_METHOD(kLogDeviceMethods || kLogNotifyMethods);
  FX_CHECK(state_ != State::DeviceInitializing);

  auto control_notify = GetControlNotify();
  if (!control_notify) {
    ADR_LOG_METHOD(kLogNotifyMethods) << "already not controlled";
    return false;
  }

  control_notify_.reset();
  // We don't remove our ControlNotify from the observer list: we wait for it to self-invalidate.

  SetState(State::DeviceInitialized);
  return true;
}

bool Device::AddObserver(std::shared_ptr<ObserverNotify> observer_to_add) {
  ADR_LOG_METHOD(kLogDeviceMethods || kLogNotifyMethods) << " (" << observer_to_add << ")";

  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << ": unhealthy, cannot be observed";
    return false;
  }
  for (auto weak_obs = observers_.begin(); weak_obs < observers_.end(); ++weak_obs) {
    if (auto observer = weak_obs->lock(); observer) {
      if (observer == observer_to_add) {
        // Because observers are not explicitly dropped (they are weakly held and allowed to
        // self-invalidate), this can occur if a Control is dropped then immediately readded.
        // This is OK because the outcome is ultimately correct (it is still in the vector).
        FX_LOGS(WARNING) << "Device(" << this << ")::AddObserver: observer cannot be re-added";
        return false;
      }
    }
  }
  observers_.push_back(observer_to_add);

  // For this new observer, "catch it up" on the current state.
  //
  // Current signalprocessing topology.

  // ElementState for each signalprocessing element.

  // GainState -- if StreamConfig.
  if (device_type_ == fuchsia_audio_device::DeviceType::kInput ||
      device_type_ == fuchsia_audio_device::DeviceType::kOutput) {
    observer_to_add->GainStateChanged({{
        .gain_db = *gain_state_->gain_db(),
        .muted = gain_state_->muted().value_or(false),
        .agc_enabled = gain_state_->agc_enabled().value_or(false),
    }});
  }

  // PlugState -- if Codec or StreamConfig.
  if (device_type_ == fuchsia_audio_device::DeviceType::kCodec ||
      device_type_ == fuchsia_audio_device::DeviceType::kInput ||
      device_type_ == fuchsia_audio_device::DeviceType::kOutput) {
    observer_to_add->PlugStateChanged(*plug_state_->plugged()
                                          ? fuchsia_audio_device::PlugState::kPlugged
                                          : fuchsia_audio_device::PlugState::kUnplugged,
                                      zx::time(*plug_state_->plug_state_time()));
  }

  LogObjectCounts();

  SetState(State::DeviceInitialized);
  return true;
}

// The Device dropped the driver RingBuffer FIDL. Notify any clients.
void Device::DeviceDroppedRingBuffer() {
  ADR_LOG_METHOD(kLogDeviceState);

  // This is distinct from DropRingBuffer in case we must notify our RingBuffer (via our Control).
  // We do so if we have 1) a Control and 2) a driver_format_ (thus a client-configured RingBuffer).
  if (auto notify = GetControlNotify(); notify && driver_format_) {
    notify->DeviceDroppedRingBuffer();
  }
  DropRingBuffer();
}

// Whether client- or device-originated, reset any state associated with an active RingBuffer.
void Device::DropRingBuffer() {
  ADR_LOG_METHOD(kLogDeviceState);

  if (state_ != State::Error) {
    SetState(State::DeviceInitialized);
  }

  // If we've already cleaned out any state with the underlying driver RingBuffer, then we're done.
  if (!ring_buffer_client_.has_value() || !ring_buffer_client_->is_valid()) {
    return;
  }

  // Revert all configuration state related to the ring buffer.
  //
  start_time_.reset();  // Pause, if we are started.

  requested_ring_buffer_bytes_.reset();  // User must call CreateRingBuffer again, leading to ...
  create_ring_buffer_callback_ = nullptr;

  driver_format_.reset();  // ... our re-calling ConnectToRingBufferFidl ...
  vmo_format_ = {};
  num_ring_buffer_frames_.reset();  // ... and GetVmo ...
  ring_buffer_vmo_.reset();

  ring_buffer_properties_.reset();  // ... and GetProperties ...

  delay_info_.reset();   // ... and WatchDelayInfo ...
  bytes_per_frame_ = 0;  // (.. which calls CalculateRequiredRingBufferSizes ...)
  requested_ring_buffer_frames_ = 0u;
  ring_buffer_producer_bytes_ = 0;
  ring_buffer_consumer_bytes_ = 0;

  active_channels_bitmask_ = 0;  // ... and SetActiveChannels.
  active_channels_set_time_.reset();

  // Clear our FIDL connection to the driver RingBuffer.
  (void)ring_buffer_client_->UnbindMaybeGetEndpoint();
  ring_buffer_client_.reset();
}

void Device::SetError(zx_status_t error) {
  ADR_LOG_METHOD(kLogDeviceState) << ": " << error;
  state_ = State::Error;
}

void Device::SetState(State state) {
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; ignoring this";
    return;
  }

  ADR_LOG_METHOD(kLogDeviceState) << state;
  state_ = state;
}

void Device::Initialize() {
  ADR_LOG_METHOD(kLogDeviceMethods);

  if (device_type_ == fuchsia_audio_device::DeviceType::kCodec) {
    RetrieveCodecProperties();
    RetrieveCodecHealthState();
    RetrieveSignalProcessingState();
    RetrieveCodecDaiFormatSets();
    RetrieveCodecPlugState();
  } else if (device_type_ == fuchsia_audio_device::DeviceType::kInput ||
             device_type_ == fuchsia_audio_device::DeviceType::kOutput) {
    RetrieveStreamProperties();
    RetrieveStreamHealthState();
    RetrieveSignalProcessingState();
    RetrieveStreamRingBufferFormatSets();
    RetrieveStreamPlugState();
    RetrieveGainState();
  } else {
    FX_LOGS(WARNING) << "Different device type: " << device_type_;
  }
}

// This method also sets OnError, so it is not just for logging.
// Use this when the Result might contain a domain_error or framework_error.
template <typename ResultT>
bool Device::LogResultError(const ResultT& result, const char* debug_context) {
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << debug_context << ": device already has an error";
    return true;
  }
  if (result.is_error()) {
    if (result.error_value().is_framework_error()) {
      if (result.error_value().framework_error().is_canceled() ||
          result.error_value().framework_error().is_peer_closed()) {
        ADR_LOG_METHOD(kLogCodecFidlResponses || kLogStreamConfigFidlResponses)
            << debug_context << ": will take no action on " << result.error_value();
      } else {
        FX_LOGS(ERROR) << debug_context << " failed: " << result.error_value() << ")";
        OnError(result.error_value().framework_error().status());
      }
    } else {
      OnError(ZX_ERR_INTERNAL);
    }
  }
  return result.is_error();
}

// This method also sets OnError, so it is not just for logging.
// Use this when the Result error can only be a framework_error.
template <typename ResultT>
bool Device::LogResultFrameworkError(const ResultT& result, const char* debug_context) {
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << debug_context << ": device already has an error";
    return true;
  }
  if (result.is_error()) {
    if (result.error_value().is_canceled() || result.error_value().is_peer_closed()) {
      ADR_LOG_METHOD(kLogCodecFidlResponses || kLogStreamConfigFidlResponses)
          << debug_context << ": will take no action on " << result.error_value();
    } else {
      FX_LOGS(ERROR) << debug_context << " failed: " << result.error_value().status() << " ("
                     << result.error_value() << ")";
      OnError(result.error_value().status());
    }
  }
  return result.is_error();
}

void Device::RetrieveStreamProperties() {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/42064765): handle command timeouts

  (*stream_config_client_)
      ->GetProperties()
      .Then([this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetProperties>& result) {
        if (LogResultFrameworkError(result, "GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "StreamConfig/GetProperties: success";
        auto status = ValidateStreamProperties(result->properties());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        FX_CHECK(!has_stream_config_properties())
            << "StreamConfig/GetProperties response: stream_config_properties_ already set";
        stream_config_properties_ = result->properties();
        SanitizeStreamPropertiesStrings(stream_config_properties_);
        // We have our clock domain now. Create the device clock.
        CreateDeviceClock();

        OnInitializationResponse();
      });
}

// Some drivers return manufacturer or product strings with embedded '\0' characters. Trim them.
void Device::SanitizeStreamPropertiesStrings(
    std::optional<fuchsia_hardware_audio::StreamProperties>& stream_properties) {
  if (!stream_properties) {
    FX_LOGS(ERROR) << __func__ << " called with unspecified StreamProperties";
    return;
  }

  if (stream_properties->manufacturer()) {
    stream_properties->manufacturer(stream_properties->manufacturer()->substr(
        0, std::min<uint64_t>(stream_properties->manufacturer()->find('\0'),
                              fuchsia_hardware_audio::kMaxUiStringSize - 1)));
  }
  if (stream_properties->product()) {
    stream_properties->product(stream_properties->product()->substr(
        0, std::min<uint64_t>(stream_properties->product()->find('\0'),
                              fuchsia_hardware_audio::kMaxUiStringSize - 1)));
  }
}

void Device::RetrieveCodecProperties() {
  ADR_LOG_METHOD(kLogCodecFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts

  (*codec_client_)
      ->GetProperties()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Codec::GetProperties>& result) {
        if (LogResultFrameworkError(result, "GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/GetProperties: success";
        auto status = ValidateCodecProperties(result->properties());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        FX_CHECK(!has_codec_properties())
            << "Codec/GetProperties response: codec_properties_ already set";
        codec_properties_ = result->properties();
        SanitizeCodecPropertiesStrings(codec_properties_);

        OnInitializationResponse();
      });
}

void Device::SanitizeCodecPropertiesStrings(
    std::optional<fuchsia_hardware_audio::CodecProperties>& codec_properties) {
  if (!codec_properties) {
    FX_LOGS(ERROR) << __func__ << " called with unspecified CodecProperties";
    return;
  }

  if (codec_properties->manufacturer()) {
    codec_properties->manufacturer(codec_properties->manufacturer()->substr(
        0, std::min<uint64_t>(codec_properties->manufacturer()->find('\0'),
                              fuchsia_hardware_audio::kMaxUiStringSize - 1)));
  }
  if (codec_properties->product()) {
    codec_properties->product(codec_properties->product()->substr(
        0, std::min<uint64_t>(codec_properties->product()->find('\0'),
                              fuchsia_hardware_audio::kMaxUiStringSize - 1)));
  }
}

void Device::RetrieveStreamRingBufferFormatSets() {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);

  RetrieveRingBufferFormatSets(
      [this](const std::vector<fuchsia_hardware_audio::SupportedFormats>& ring_buffer_format_sets) {
        if (state_ == State::Error) {
          ADR_WARN_OBJECT() << "device has an error while retrieving initial RingBuffer formats";
          return;
        }

        ring_buffer_format_sets_ = std::vector<fuchsia_hardware_audio::SupportedFormats>();
        for (const auto& rb_format_set : ring_buffer_format_sets) {
          ring_buffer_format_sets_->emplace_back(rb_format_set);
        }

        // Required for StreamConfig and Dai, absent for Codec and Composite.
        translated_ring_buffer_format_sets_ =
            TranslateRingBufferFormatSets(*ring_buffer_format_sets_);
        if (translated_ring_buffer_format_sets_.empty()) {
          ADR_WARN_OBJECT() << "RingBuffer format sets could not be translated";
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        ring_buffer_format_sets_retrieved_ = true;
        OnInitializationResponse();
      });
}

void Device::RetrieveRingBufferFormatSets(
    fit::callback<void(std::vector<fuchsia_hardware_audio::SupportedFormats>)>
        ring_buffer_format_sets_callback) {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls || kLogRingBufferMethods);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/42064765): handle command timeouts

  (*stream_config_client_)
      ->GetSupportedFormats()
      .Then([this, rb_formats_callback = std::move(ring_buffer_format_sets_callback)](
                fidl::Result<fuchsia_hardware_audio::StreamConfig::GetSupportedFormats>&
                    result) mutable {
        if (LogResultFrameworkError(result, "GetSupportedFormats response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
            << "StreamConfig/GetSupportedFormats: success";
        auto status = ValidateRingBufferFormatSets(result->supported_formats());
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        rb_formats_callback(result->supported_formats());
      });
}

void Device::RetrieveSignalProcessingState() {
  ADR_LOG_METHOD(kLogDeviceMethods);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }

  auto endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_audio_signalprocessing::SignalProcessing>();
  if (!endpoints.is_ok()) {
    ADR_WARN_METHOD() << "unable to create SignalProcessing endpoints: "
                      << endpoints.status_string();
    OnError(endpoints.status_value());
    return;
  }
  auto [sig_proc_client_end, sig_proc_server_end] = std::move(endpoints.value());

  // TODO(https://fxbug.dev/113429): handle command timeouts

  if (device_type() == fuchsia_audio_device::DeviceType::kCodec) {
    ADR_LOG_METHOD(kLogCodecFidlCalls) << "calling SignalProcessingConnect";
    if (!codec_client_->is_valid()) {
      return;
    }
    auto status = (*codec_client_)->SignalProcessingConnect(std::move(sig_proc_server_end));

    if (status.is_error()) {
      if (status.error_value().is_canceled()) {
        // These indicate that we are already shutting down, so they aren't error conditions.
        ADR_LOG_METHOD(kLogCodecFidlResponses)
            << "SignalProcessingConnect response will take no action on error "
            << status.error_value().FormatDescription();
        return;
      }

      FX_PLOGS(ERROR, status.error_value().status()) << __func__ << " returned error:";
      OnError(status.error_value().status());
      return;
    }
  } else if (device_type() == fuchsia_audio_device::DeviceType::kInput ||
             device_type() == fuchsia_audio_device::DeviceType::kOutput) {
    ADR_LOG_METHOD(kLogStreamConfigFidlCalls) << "calling SignalProcessingConnect";
    if (!stream_config_client_->is_valid()) {
      return;
    }

    auto status = (*stream_config_client_)->SignalProcessingConnect(std::move(sig_proc_server_end));

    if (status.is_error()) {
      if (status.error_value().is_canceled()) {
        // These indicate that we are already shutting down, so they aren't error conditions.
        ADR_LOG_METHOD(kLogStreamConfigFidlResponses)
            << "SignalProcessingConnect response will take no action on error "
            << status.error_value().FormatDescription();
        return;
      }

      FX_PLOGS(ERROR, status.error_value().status()) << __func__ << " returned error:";
      OnError(status.error_value().status());
      return;
    }
  }

  sig_proc_client_.emplace();
  sig_proc_handler_ = {this, "SignalProcessing"};
  sig_proc_client_->Bind(std::move(sig_proc_client_end), dispatcher_, &sig_proc_handler_);

  RetrieveSignalProcessingElements();
}

void Device::RetrieveSignalProcessingElements() {
  ADR_LOG_METHOD(kLogSignalProcessingFidlCalls);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }

  (*sig_proc_client_)
      ->GetElements()
      .Then(
          [this](
              fidl::Result<fuchsia_hardware_audio_signalprocessing::SignalProcessing::GetElements>&
                  result) {
            std::string context("signalprocessing::GetElements response");
            if (result.is_error() && result.error_value().is_domain_error() &&
                result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED) {
              ADR_LOG_OBJECT(kLogSignalProcessingFidlResponses) << context << ": NOT_SUPPORTED";

              SetSignalProcessingSupported(false);
              return;
            }
            if (LogResultError(result, context.c_str())) {
              return;
            }

            ADR_LOG_OBJECT(kLogSignalProcessingFidlResponses) << context;
            auto status = ValidateElements(result->processing_elements());
            if (status != ZX_OK) {
              OnError(status);
              return;
            }

            sig_proc_elements_ = result->processing_elements();
            sig_proc_element_map_ = MapElements(sig_proc_elements_);
            if (sig_proc_element_map_.empty()) {
              OnError(ZX_ERR_INVALID_ARGS);
              return;
            }
            RetrieveSignalProcessingTopologies();
          });
}

void Device::RetrieveSignalProcessingTopologies() {
  ADR_LOG_METHOD(kLogSignalProcessingFidlCalls);
  if (state_ == State::Error || (checked_for_signalprocessing() && !supports_signalprocessing())) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  FX_DCHECK(sig_proc_client_->is_valid());

  (*sig_proc_client_)
      ->GetTopologies()
      .Then([this](fidl::Result<
                   fuchsia_hardware_audio_signalprocessing::SignalProcessing::GetTopologies>&
                       result) {
        std::string context("signalprocessing::GetTopologies response");
        if (result.is_error() && result.error_value().is_domain_error() &&
            result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED) {
          SetSignalProcessingSupported(false);
          return;
        }
        if (LogResultError(result, context.c_str())) {
          return;
        }

        ADR_LOG_OBJECT(kLogSignalProcessingFidlResponses) << context;
        auto status = ValidateTopologies(result->topologies(), sig_proc_element_map_);
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        sig_proc_topologies_ = result->topologies();
        sig_proc_topology_map_ = MapTopologies(sig_proc_topologies_);
        if (sig_proc_topology_map_.empty()) {
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        SetSignalProcessingSupported(true);

        // Now we know we support signalprocessing: query the current topology/element states.
        RetrieveCurrentTopology();
        RetrieveCurrentElementStates();
      });
}

void Device::RetrieveCurrentTopology() {
  ADR_LOG_METHOD(kLogSignalProcessingFidlCalls);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  FX_DCHECK(sig_proc_client_->is_valid());

  (*sig_proc_client_)
      ->WatchTopology()
      .Then([this](fidl::Result<
                   fuchsia_hardware_audio_signalprocessing::SignalProcessing::WatchTopology>&
                       result) {
        std::string context("signalprocessing::WatchTopology response");
        if (LogResultFrameworkError(result, context.c_str())) {
          return;
        }

        TopologyId topology_id = result->topology_id();
        auto match = sig_proc_topology_map_.find(topology_id);
        if (match == sig_proc_topology_map_.end()) {
          ADR_WARN_OBJECT() << context << ": topology_id " << topology_id << " not found";
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        if (!current_topology_id_.has_value() || *current_topology_id_ != topology_id) {
          current_topology_id_ = topology_id;
        }
      });
}

void Device::RetrieveCurrentElementStates() {
  ADR_LOG_METHOD(kLogSignalProcessingFidlCalls);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  FX_DCHECK(sig_proc_client_->is_valid());

  for (auto& element_pair : sig_proc_element_map_) {
    const auto& element = element_pair.second;
    (*sig_proc_client_)
        ->WatchElementState({{.processing_element_id = element_pair.first}})
        .Then([this, element](
                  fidl::Result<
                      fuchsia_hardware_audio_signalprocessing::SignalProcessing::WatchElementState>&
                      result) {
          std::string context("signalprocessing::WatchElementState response");
          if (LogResultFrameworkError(result, context.c_str())) {
            return;
          }

          auto element_state = result->state();
          if (auto status = ValidateElementState(element_state, element); status != ZX_OK) {
            OnError(status);
            return;
          }

          // Save the initial element state, for Observers
        });
  }
}

void Device::RetrieveCodecDaiFormatSets() {
  ADR_LOG_METHOD(kLogCodecFidlCalls);
  RetrieveDaiFormatSets(
      [this](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_format_sets) {
        if (state_ == State::Error) {
          ADR_WARN_METHOD() << "device already has an error";
          return;
        }
        dai_format_sets_ = std::vector<fuchsia_hardware_audio::DaiSupportedFormats>();
        for (const auto& dai_format_set : dai_format_sets) {
          dai_format_sets_->emplace_back(dai_format_set);
        }
        dai_format_sets_retrieved_ = true;
        OnInitializationResponse();
      });
}

void Device::RetrieveDaiFormatSets(
    fit::callback<void(std::vector<fuchsia_hardware_audio::DaiSupportedFormats>)>
        dai_format_sets_callback) {
  ADR_LOG_METHOD(kLogCodecFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts

  (*codec_client_)
      ->GetDaiFormats()
      .Then([this, callback = std::move(dai_format_sets_callback)](
                fidl::Result<fuchsia_hardware_audio::Codec::GetDaiFormats>& result) mutable {
        if (LogResultError(result, "GetDaiFormats response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/GetDaiFormats: success";

        if (auto status = ValidateDaiFormatSets(result->formats()); status != ZX_OK) {
          OnError(status);
          return;
        }
        callback(result->formats());
      });
}

void Device::RetrieveGainState() {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  if (!has_gain_state()) {
    // TODO(https://fxbug.dev/42064765): handle command timeout on initial (not subsequent) watches.
  }

  (*stream_config_client_)
      ->WatchGainState()
      .Then([this](fidl::Result<fuchsia_hardware_audio::StreamConfig::WatchGainState>& result) {
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
          ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchGainState received initial value";
          OnInitializationResponse();
        } else {
          ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchGainState received update";
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

void Device::RetrieveStreamPlugState() {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  if (!has_plug_state()) {
    // TODO(https://fxbug.dev/42064765): handle command timeouts (but not on subsequent watches)
  }

  (*stream_config_client_)
      ->WatchPlugState()
      .Then([this](fidl::Result<fuchsia_hardware_audio::StreamConfig::WatchPlugState>& result) {
        if (LogResultFrameworkError(result, "PlugState response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses) << "WatchPlugState response";
        std::optional<fuchsia_hardware_audio::PlugDetectCapabilities> plug_detect_capabilities;
        if (has_stream_config_properties()) {
          plug_detect_capabilities = stream_config_properties_->plug_detect_capabilities();
        }
        auto status = ValidatePlugState(result->plug_state(), plug_detect_capabilities);
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        auto old_plug_state = plug_state_;
        plug_state_ = result->plug_state();

        if (!old_plug_state) {
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
        RetrieveStreamPlugState();
      });
}

void Device::RetrieveCodecPlugState() {
  ADR_LOG_METHOD(kLogCodecFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  if (!has_plug_state()) {
    // TODO(https://fxbug.dev/113429): handle command timeouts (but not on subsequent watches)
  }

  (*codec_client_)
      ->WatchPlugState()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Codec::WatchPlugState>& result) {
        if (LogResultFrameworkError(result, "Codec::PlugState response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec::WatchPlugState response";
        std::optional<fuchsia_hardware_audio::PlugDetectCapabilities> plug_detect_capabilities;
        if (has_codec_properties()) {
          plug_detect_capabilities = codec_properties_->plug_detect_capabilities();
        }
        auto status = ValidatePlugState(result->plug_state(), plug_detect_capabilities);
        if (status != ZX_OK) {
          OnError(status);
          return;
        }

        auto old_plug_state = plug_state_;
        plug_state_ = result->plug_state();

        if (!old_plug_state) {
          ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec::WatchPlugState received initial value";
          OnInitializationResponse();
        } else {
          ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec::WatchPlugState received update";
          ForEachObserver([plug_state = *plug_state_](auto obs) {
            obs->PlugStateChanged(plug_state.plugged().value_or(true)
                                      ? fuchsia_audio_device::PlugState::kPlugged
                                      : fuchsia_audio_device::PlugState::kUnplugged,
                                  zx::time(*plug_state.plug_state_time()));
          });
        }
        // Kick off the next watch.
        RetrieveCodecPlugState();
      });
}

// TODO(https://fxbug.dev/42068381): Decide when we proactively call GetHealthState, if at all.
void Device::RetrieveStreamHealthState() {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/42064765): handle command timeouts

  (*stream_config_client_)
      ->GetHealthState()
      .Then([this](fidl::Result<fuchsia_hardware_audio::StreamConfig::GetHealthState>& result) {
        if (LogResultFrameworkError(result, "HealthState response")) {
          return;
        }

        auto old_health_state = health_state_;

        // An empty health state is permitted; it still indicates that the driver is responsive.
        health_state_ = result->state().healthy().value_or(true);
        // ...but if the driver actually self-reported as unhealthy, this is a problem.
        if (!*health_state_) {
          FX_LOGS(WARNING)
              << "RetrieveStreamConfigHealthState response: .healthy is FALSE (unhealthy)";
          OnError(ZX_ERR_IO);
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
            << "RetrieveStreamConfigHealthState response: healthy";
        if (!old_health_state) {
          OnInitializationResponse();
        }
      });
}

void Device::RetrieveCodecHealthState() {
  ADR_LOG_METHOD(kLogCodecFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts, because that's the most likely
  // indicator of an unhealthy driver/device.

  (*codec_client_)
      ->GetHealthState()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Codec::GetHealthState>& result) {
        if (LogResultFrameworkError(result, "HealthState response")) {
          return;
        }

        auto old_health_state = health_state_;

        // An empty health state is permitted; it still indicates that the driver is responsive.
        health_state_ = result->state().healthy().value_or(true);
        // ...but if the driver actually self-reported as unhealthy, this is a problem.
        if (!*health_state_) {
          FX_LOGS(WARNING) << "RetrieveCodecHealthState response: .healthy is FALSE (unhealthy)";
          OnError(ZX_ERR_IO);
          return;
        }

        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "RetrieveCodecHealthState response: healthy";
        if (!old_health_state) {
          OnInitializationResponse();
        }
      });
}

// Return a fuchsia_audio_device::Info object based on this device's member values.
// Required fields (guaranteed for the caller) include: token_id, device_type, device_name.
// Other fields are required for some driver types but optional or absent for others.
fuchsia_audio_device::Info Device::CreateDeviceInfo() {
  ADR_LOG_METHOD(kLogDeviceMethods);

  auto info = fuchsia_audio_device::Info{{
      // Required for all device types:
      .token_id = token_id_,
      .device_type = device_type_,
      .device_name = name_,
  }};
  // Required for Composite; optional for Codec, Dai and StreamConfig:
  if (supports_signalprocessing()) {
    info.signal_processing_elements(sig_proc_elements_);
    info.signal_processing_topologies(sig_proc_topologies_);
  }
  if (device_type_ == fuchsia_audio_device::DeviceType::kCodec) {
    // Optional for all device types.
    info.manufacturer(codec_properties_->manufacturer())
        .product(codec_properties_->product())
        // Required for Dai and StreamConfig; optional for Codec:
        .is_input(codec_properties_->is_input())
        // Required for Codec, Dai and Composite; absent for StreamConfig:
        .dai_format_sets(dai_format_sets())
        // Required for Codec and StreamConfig; absent for Composite and Dai:
        .plug_detect_caps(*codec_properties_->plug_detect_capabilities() ==
                                  fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired
                              ? fuchsia_audio_device::PlugDetectCapabilities::kHardwired
                              : fuchsia_audio_device::PlugDetectCapabilities::kPluggable);
    // Codec properties stores unique_id as a string, so we must handle as a special case.
    if (codec_properties_->unique_id()) {
      std::array<unsigned char, fuchsia_audio_device::kUniqueInstanceIdSize> uid{};
      memcpy(uid.data(), codec_properties_->unique_id()->data(),
             fuchsia_audio_device::kUniqueInstanceIdSize);
      info.unique_instance_id(uid);
    }
  }
  if (device_type_ == fuchsia_audio_device::DeviceType::kInput ||
      device_type_ == fuchsia_audio_device::DeviceType::kOutput) {
    // Optional for all device types:
    info.manufacturer(stream_config_properties_->manufacturer())
        .product(stream_config_properties_->product())
        .unique_instance_id(stream_config_properties_->unique_id())
        // Required for Dai and StreamConfig; optional for Codec:
        .is_input(stream_config_properties_->is_input())
        // Required for Dai and StreamConfig; absent for Codec and Composite:
        .ring_buffer_format_sets(translated_ring_buffer_format_sets_)
        // Required for StreamConfig; absent for Codec, Composite and Dai:
        .gain_caps(fuchsia_audio_device::GainCapabilities{{
            .min_gain_db = stream_config_properties_->min_gain_db(),
            .max_gain_db = stream_config_properties_->max_gain_db(),
            .gain_step_db = stream_config_properties_->gain_step_db(),
            .can_mute = stream_config_properties_->can_mute(),
            .can_agc = stream_config_properties_->can_agc(),
        }})
        // Required for Codec and StreamConfig; absent for Composite and Dai:
        .plug_detect_caps(*stream_config_properties_->plug_detect_capabilities() ==
                                  fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired
                              ? fuchsia_audio_device::PlugDetectCapabilities::kHardwired
                              : fuchsia_audio_device::PlugDetectCapabilities::kPluggable)
        // Required for Composite, Dai and StreamConfig; absent for Codec:
        .clock_domain(stream_config_properties_->clock_domain());
  }

  return info;
}

void Device::SetDeviceInfo() {
  ADR_LOG_METHOD(kLogDeviceMethods);

  device_info_ = CreateDeviceInfo();
  ValidateDeviceInfo(*device_info_);
}

void Device::CreateDeviceClock() {
  ADR_LOG_METHOD(kLogDeviceMethods);
  FX_CHECK(stream_config_properties_->clock_domain()) << "Clock domain is required";

  device_clock_ = RealClock::CreateFromMonotonic("'" + name_ + "' device clock",
                                                 *stream_config_properties_->clock_domain(),
                                                 (*stream_config_properties_->clock_domain() !=
                                                  fuchsia_hardware_audio::kClockDomainMonotonic));
}

// Create a duplicate handle to our clock with limited rights. We can transfer it to a client who
// can only read and duplicate. Specifically, they cannot change this clock's rate or offset.
zx::result<zx::clock> Device::GetReadOnlyClock() const {
  ADR_LOG_METHOD(kLogDeviceMethods);

  auto dupe_clock = device_clock_->DuplicateZxClockReadOnly();
  if (!dupe_clock) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  return zx::ok(std::move(*dupe_clock));
}

// Determine the full fuchsia_hardware_audio::Format needed for ConnectRingBufferFidl.
// This method expects that the required fields are present.
std::optional<fuchsia_hardware_audio::Format> Device::SupportedDriverFormatForClientFormat(
    // TODO(https://fxbug.dev/42069015): Consider using media_audio::Format internally.
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
  for (const auto& ring_buffer_format_set : *ring_buffer_format_sets_) {
    const auto pcm_format_set = *ring_buffer_format_set.pcm_supported_formats();
    if (std::count_if(
            pcm_format_set.sample_formats()->begin(), pcm_format_set.sample_formats()->end(),
            [driver_sample_format](const auto& f) { return f == driver_sample_format; }) &&
        std::count_if(pcm_format_set.bytes_per_sample()->begin(),
                      pcm_format_set.bytes_per_sample()->end(),
                      [bytes_per_sample](const auto& bs) { return bs == bytes_per_sample; }) &&
        std::count_if(pcm_format_set.frame_rates()->begin(), pcm_format_set.frame_rates()->end(),
                      [frame_rate](const auto& fr) { return fr == frame_rate; }) &&
        std::count_if(pcm_format_set.channel_sets()->begin(), pcm_format_set.channel_sets()->end(),
                      [channel_count](const fuchsia_hardware_audio::ChannelSet& cs) {
                        return cs.attributes()->size() == channel_count;
                      })) {
      std::for_each(pcm_format_set.valid_bits_per_sample()->begin(),
                    pcm_format_set.valid_bits_per_sample()->end(),
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

  ADR_LOG_METHOD(kLogRingBufferFidlResponseValues)
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

bool Device::SetGain(fuchsia_hardware_audio::GainState& gain_state) {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Device must be allocated before this method can be called";
    return false;
  }

  auto status = (*stream_config_client_)->SetGain(std::move(gain_state));
  if (status.is_error()) {
    if (status.error_value().is_canceled()) {
      // These indicate that we are already shutting down, so they aren't error conditions.
      ADR_LOG_METHOD(kLogStreamConfigFidlResponses)
          << "SetGain response will take no action on error " << status.error_value();

      return false;
    }

    FX_PLOGS(ERROR, status.error_value().status()) << __func__ << " returned error:";
    OnError(status.error_value().status());
    return false;
  }

  ADR_LOG_METHOD(kLogStreamConfigFidlResponses) << " is_ok";

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
    control_notify_.reset();
    LogObjectCounts();
  }

  return sh_ptr_control;
}

bool Device::CodecSetDaiFormat(const fuchsia_hardware_audio::DaiFormat& dai_format) {
  if (device_type_ != fuchsia_audio_device::DeviceType::kCodec) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot SetDaiFormat";
    return false;
  }

  ADR_LOG_METHOD(kLogCodecFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts
  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Codec must be controlled before SetDaiFormat can be called";
    return false;
  }
  if (ValidateDaiFormat(dai_format) != ZX_OK) {
    ADR_WARN_METHOD() << "Invalid dai_format -- cannot SetDaiFormat";
    return false;
  }

  auto new_dai_format = dai_format;
  (*codec_client_)
      ->SetDaiFormat(dai_format)
      .Then([this, dai_format = std::move(new_dai_format)](
                fidl::Result<fuchsia_hardware_audio::Codec::SetDaiFormat>& result) {
        if (state_ == State::Error) {
          ADR_WARN_OBJECT() << "Codec/SetDaiFormat response: device already has an error";
          if (auto notify = GetControlNotify(); notify) {
            notify->DaiFormatNotSet(dai_format, ZX_ERR_INTERNAL);
          }
          return;
        }

        if (!result.is_ok()) {
          // These types of errors don't lead us to mark the device as in Error state.
          if (result.error_value().is_domain_error() &&
              (result.error_value().domain_error() == ZX_ERR_INVALID_ARGS ||
               result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED)) {
            ADR_WARN_OBJECT()
                << "Codec/SetDaiFormat response: ZX_ERR_INVALID_ARGS or ZX_ERR_NOT_SUPPORTED";
          } else {
            LogResultError(result, "SetDaiFormat response");
          }
          if (auto notify = GetControlNotify(); notify) {
            notify->DaiFormatNotSet(dai_format,
                                    result.error_value().is_domain_error()
                                        ? result.error_value().domain_error()
                                        : result.error_value().framework_error().status());
          }

          return;
        }

        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/SetDaiFormat: success";
        zx_status_t status = ValidateCodecFormatInfo(result->state());
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "Codec/SetDaiFormat error: " << result.error_value();
          OnError(status);
          if (auto notify = GetControlNotify(); notify) {
            notify->DaiFormatNotSet(dai_format, status);
          }
          return;
        }

        if (codec_format_ && codec_format_->dai_format == dai_format &&
            codec_format_->codec_format_info == result->state()) {
          // No change
          return;
        }

        // Reset Start state and DaiFormat (if this is a change) and notify our controlling entity.
        bool was_started = codec_start_state_.started;
        if (was_started) {
          codec_start_state_.started = false;
          codec_start_state_ = CodecStartState{false, zx::clock::get_monotonic()};
        }
        codec_format_ = CodecFormat{dai_format, result->state()};
        if (auto notify = GetControlNotify(); notify) {
          if (was_started) {
            notify->CodecStopped(codec_start_state_.start_stop_time);
          }
          notify->DaiFormatChanged(codec_format_->dai_format, codec_format_->codec_format_info);
        }
      });

  return true;
}

bool Device::CodecReset() {
  if (device_type_ != fuchsia_audio_device::DeviceType::kCodec) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot Reset";
    return false;
  }

  ADR_LOG_METHOD(kLogCodecFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts
  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Codec must be controlled before Reset can be called";
    return false;
  }

  (*codec_client_)
      ->Reset()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Codec::Reset>& result) {
        if (LogResultFrameworkError(result, "Reset response")) {
          return;
        }
        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/Reset: success";

        // Reset to Stopped (if Started), even if no ControlNotify listens for notifications.
        if (codec_start_state_.started) {
          codec_start_state_.started = false;
          codec_start_state_.start_stop_time = zx::clock::get_monotonic();
          if (auto notify = GetControlNotify(); notify) {
            notify->CodecStopped(codec_start_state_.start_stop_time);
          }
        }

        // Reset our DaiFormat (if set), even if no ControlNotify listens for notifications.
        if (codec_format_) {
          codec_format_.reset();
          if (auto notify = GetControlNotify(); notify) {
            notify->DaiFormatChanged(std::nullopt, std::nullopt);
          }
        }

        // TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).
        // When implemented, reset the signalprocessing topology and all elements, here.
      });

  return true;
}

bool Device::CodecStart() {
  if (device_type_ != fuchsia_audio_device::DeviceType::kCodec) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot Start";
    return false;
  }

  ADR_LOG_METHOD(kLogCodecFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts
  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Codec must be controlled before Start can be called";
    return false;
  }
  if (!codec_format_) {
    ADR_WARN_METHOD() << "Format must be set before Start can be called";
    return false;
  }

  if (codec_start_state_.started) {
    ADR_LOG_METHOD(kLogCodecFidlCalls) << "Codec is already started; ignoring CodecStart command";
    return true;
  }

  (*codec_client_)
      ->Start()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Codec::Start>& result) {
        if (LogResultFrameworkError(result, "Start response")) {
          if (auto notify = GetControlNotify(); notify) {
            notify->CodecNotStarted();
          }
          return;
        }

        ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/Start: success";

        // Notify our controlling entity, if this was a change.
        if (!codec_start_state_.started ||
            codec_start_state_.start_stop_time.get() <= result->start_time()) {
          codec_start_state_.started = true;
          codec_start_state_.start_stop_time = zx::time(result->start_time());
          if (auto notify = GetControlNotify(); notify) {
            notify->CodecStarted(codec_start_state_.start_stop_time);
          }
        }
      });

  return true;
}

bool Device::CodecStop() {
  if (device_type_ != fuchsia_audio_device::DeviceType::kCodec) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot Stop";
    return false;
  }

  ADR_LOG_METHOD(kLogCodecFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts
  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Codec must be controlled before Stop can be called";
    return false;
  }
  if (!codec_format_) {
    ADR_WARN_METHOD() << "Format must be set before Stop can be called";
    return false;
  }

  if (!codec_start_state_.started) {
    ADR_LOG_METHOD(kLogCodecFidlCalls) << "Codec is already stopped; ignoring CodecStop command";
    return true;
  }

  (*codec_client_)->Stop().Then([this](fidl::Result<fuchsia_hardware_audio::Codec::Stop>& result) {
    if (LogResultFrameworkError(result, "Stop response")) {
      if (auto notify = GetControlNotify(); notify) {
        notify->CodecNotStopped();
      }
      return;
    }

    ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/Stop: success";

    // Notify our controlling entity, if this was a change.
    if (codec_start_state_.started ||
        codec_start_state_.start_stop_time.get() <= result->stop_time()) {
      codec_start_state_.started = false;
      codec_start_state_.start_stop_time = zx::time(result->stop_time());
      if (auto notify = GetControlNotify(); notify) {
        notify->CodecStopped(codec_start_state_.start_stop_time);
      }
    }
  });

  return true;
}

bool Device::CreateRingBuffer(const fuchsia_hardware_audio::Format& format,
                              uint32_t requested_ring_buffer_bytes,
                              fit::callback<void(RingBufferInfo)> create_ring_buffer_callback) {
  ADR_LOG_METHOD(kLogRingBufferMethods);
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
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }

  auto status = ValidateRingBufferFormat(driver_format);
  if (status != ZX_OK) {
    OnError(status);
    return false;
  }

  auto bytes_per_sample = driver_format.pcm_format()->bytes_per_sample();
  auto sample_format = driver_format.pcm_format()->sample_format();
  status = ValidateSampleFormatCompatibility(bytes_per_sample, sample_format);
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

  auto result =
      (*stream_config_client_)->CreateRingBuffer({driver_format, std::move(endpoints->server)});
  if (!result.is_ok()) {
    FX_PLOGS(ERROR, result.error_value().status()) << "StreamConfig/CreateRingBuffer failed";
    OnError(result.error_value().status());
    return false;
  }

  ring_buffer_client_.emplace();
  ring_buffer_handler_ = {this, "RingBuffer"};
  ring_buffer_client_->Bind(std::move(endpoints->client), dispatcher_, &ring_buffer_handler_);

  std::optional<fuchsia_audio::SampleType> sample_type;
  if (bytes_per_sample == 1 &&
      sample_format == fuchsia_hardware_audio::SampleFormat::kPcmUnsigned) {
    sample_type = fuchsia_audio::SampleType::kUint8;
  } else if (bytes_per_sample == 2 &&
             sample_format == fuchsia_hardware_audio::SampleFormat::kPcmSigned) {
    sample_type = fuchsia_audio::SampleType::kInt16;
  } else if (bytes_per_sample == 4 &&
             sample_format == fuchsia_hardware_audio::SampleFormat::kPcmSigned) {
    sample_type = fuchsia_audio::SampleType::kInt32;
  } else if (bytes_per_sample == 4 &&
             sample_format == fuchsia_hardware_audio::SampleFormat::kPcmFloat) {
    sample_type = fuchsia_audio::SampleType::kFloat32;
  } else if (bytes_per_sample == 8 &&
             sample_format == fuchsia_hardware_audio::SampleFormat::kPcmFloat) {
    sample_type = fuchsia_audio::SampleType::kFloat64;
  }
  FX_CHECK(sample_type)
      << "Invalid sample format was not detected in ValidateSampleFormatCompatibility";

  driver_format_ = driver_format;  // This contains valid_bits_per_sample.
  vmo_format_ = {{
      .sample_type = *sample_type,
      .channel_count = driver_format.pcm_format()->number_of_channels(),
      .frames_per_second = driver_format.pcm_format()->frame_rate(),
      // TODO(https://fxbug.dev/42168795): handle .channel_layout, when communicated from driver.
  }};

  active_channels_bitmask_ = (1 << *vmo_format_.channel_count()) - 1;
  SetActiveChannels(active_channels_bitmask_, [this](zx::result<zx::time> result) {
    if (result.is_ok()) {
      ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/SetActiveChannels IS supported";
      return;
    }
    if (result.status_value() == ZX_ERR_NOT_SUPPORTED) {
      ADR_LOG_OBJECT(kLogRingBufferFidlResponses)
          << "RingBuffer/SetActiveChannels IS NOT supported";
      return;
    }
    ADR_WARN_OBJECT() << "RingBuffer/SetActiveChannels returned error: " << result.status_string();
    OnError(result.status_value());
  });
  SetState(State::CreatingRingBuffer);

  return true;
}

void Device::RetrieveRingBufferProperties() {
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.has_value() && ring_buffer_client_->is_valid());

  (*ring_buffer_client_)
      ->GetProperties()
      .Then([this](fidl::Result<fuchsia_hardware_audio::RingBuffer::GetProperties>& result) {
        if (LogResultFrameworkError(result, "RingBuffer/GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/GetProperties: success";
        auto status = ValidateRingBufferProperties(result->properties());
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "RingBuffer/GetProperties error: " << result.error_value();
          OnError(status);
          return;
        }

        ring_buffer_properties_ = result->properties();
        CheckForRingBufferReady();
      });
}

void Device::RetrieveDelayInfo() {
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.has_value() && ring_buffer_client_->is_valid());

  (*ring_buffer_client_)
      ->WatchDelayInfo()
      .Then([this](fidl::Result<fuchsia_hardware_audio::RingBuffer::WatchDelayInfo>& result) {
        if (LogResultFrameworkError(result, "RingBuffer/WatchDelayInfo response")) {
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/WatchDelayInfo: success";

        auto status = ValidateDelayInfo(result->delay_info());
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
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.has_value() && ring_buffer_client_->is_valid());
  FX_CHECK(driver_format_);

  (*ring_buffer_client_)
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
  ADR_LOG_METHOD(kLogRingBufferFidlResponses);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
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
    ADR_WARN_METHOD() << "reference clock is not ok";
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
  ADR_LOG_METHOD(kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.has_value() && ring_buffer_client_->is_valid());

  // If we already know this device doesn't support SetActiveChannels, do nothing.
  if (!supports_set_active_channels_.value_or(true)) {
    return;
  }
  (*ring_buffer_client_)
      ->SetActiveChannels({{.active_channels_bitmask = channel_bitmask}})
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
  ADR_LOG_METHOD(kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.has_value() && ring_buffer_client_->is_valid());

  (*ring_buffer_client_)
      ->Start()
      .Then([this, callback = std::move(start_callback)](
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
  ADR_LOG_METHOD(kLogRingBufferFidlCalls);
  FX_CHECK(ring_buffer_client_.has_value() && ring_buffer_client_->is_valid());

  (*ring_buffer_client_)
      ->Stop()
      .Then([this, callback = std::move(stop_callback)](
                fidl::Result<fuchsia_hardware_audio::RingBuffer::Stop>& result) mutable {
        if (LogResultFrameworkError(result, "RingBuffer/Stop response")) {
          callback(ZX_ERR_INTERNAL);
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/Stop: success";

        start_time_.reset();
        callback(ZX_OK);
        SetState(State::RingBufferStopped);
      });
}

// Uses the VMO format and ring buffer properties, to set bytes_per_frame_,
// requested_ring_buffer_frames_, ring_buffer_consumer_bytes_ and ring_buffer_producer_bytes_.
void Device::CalculateRequiredRingBufferSizes() {
  ADR_LOG_METHOD(kLogRingBufferMethods);

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

  // TODO(https://fxbug.dev/42069012): validate this case; we don't surface an error to the caller.
  if (requested_ring_buffer_frames_ > std::numeric_limits<uint32_t>::max()) {
    ADR_WARN_METHOD() << "requested_ring_buffer_frames_ cannot exceed uint32_t::max()";
    requested_ring_buffer_frames_ = std::numeric_limits<uint32_t>::max();
  }
}

// TODO(https://fxbug.dev/42069013): implement, via RingBuffer/WatchClockRecoveryPositionInfo.
void Device::RecoverDeviceClockFromPositionInfo() { ADR_LOG_METHOD(kLogRingBufferMethods); }

// TODO(https://fxbug.dev/42069013): implement this.
void Device::StopDeviceClockRecovery() { ADR_LOG_METHOD(kLogRingBufferMethods); }

}  // namespace media_audio

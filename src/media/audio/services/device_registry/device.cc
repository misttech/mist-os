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
#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/common.h"
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

// Invoked when a RingBuffer channel drops. Device state previously was Configured/Paused/Started.
template <>
void Device::EndpointFidlErrorHandler<fuchsia_hardware_audio::RingBuffer>::on_fidl_error(
    fidl::UnbindInfo info) {
  ADR_LOG_METHOD(kLogRingBufferFidlResponses || kLogObjectLifetimes) << "(RingBuffer)";
  if (device()->state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; no device state to unwind";
  } else if (info.is_peer_closed() || info.is_user_initiated()) {
    ADR_LOG_METHOD(kLogRingBufferFidlResponses) << name() << " disconnected: " << info;
    device()->DeviceDroppedRingBuffer(element_id_);
  } else {
    ADR_WARN_METHOD() << name() << " disconnected: " << info;
    device()->OnError(info.status());
  }
  auto& ring_buffer = device()->ring_buffer_map_.find(element_id_)->second;
  ring_buffer.ring_buffer_client.reset();
  device()->ring_buffer_map_.erase(element_id_);
}

// Invoked when a SignalProcessing channel drops. This can occur during device initialization.
template <>
void Device::FidlErrorHandler<fuchsia_hardware_audio_signalprocessing::SignalProcessing>::
    on_fidl_error(fidl::UnbindInfo info) {
  ADR_LOG_METHOD(kLogRingBufferFidlResponses || kLogObjectLifetimes) << "(SignalProcessing)";
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

// Invoked when the underlying driver disconnects.
template <typename T>
void Device::FidlErrorHandler<T>::on_fidl_error(fidl::UnbindInfo info) {
  if (!info.is_peer_closed() && !info.is_user_initiated() && !info.is_dispatcher_shutdown()) {
    ADR_WARN_METHOD() << name_ << " disconnected: " << info;
    device_->OnError(info.status());
  } else {
    ADR_LOG_METHOD(kLogCodecFidlResponses || kLogCompositeFidlResponses ||
                   kLogStreamConfigFidlResponses || kLogObjectLifetimes)
        << name_ << " disconnected: " << info;
  }
  device_->OnRemoval();
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
    case fuchsia_audio_device::DeviceType::kComposite:
      composite_handler_ = {this, "Composite"};
      composite_client_ = {driver_client_.composite().take().value(), dispatcher,
                           &composite_handler_};
      break;
    case fuchsia_audio_device::DeviceType::kInput:
    case fuchsia_audio_device::DeviceType::kOutput:
      stream_config_handler_ = {this, "StreamConfig"};
      stream_config_client_ = {driver_client_.stream_config().take().value(), dispatcher,
                               &stream_config_handler_};
      break;
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

// Called during initialization, when we know definitively whether the driver supports the protocol.
// It might also be called later, if there is an error related to the signalprocessing protocol.
// If this is the first time this is called, then call `OnInitializationResponse` to unblock the
// signalprocessing-related aspect of the "wait for multiple responses" state machine.
void Device::SetSignalProcessingSupported(bool is_supported) {
  ADR_LOG_METHOD(kLogSignalProcessingFidlResponses);
  auto first_set_of_signalprocessing_support = !supports_signalprocessing_.has_value();

  supports_signalprocessing_ = is_supported;

  // Only poke the initialization state machine the FIRST time this is called.
  if (first_set_of_signalprocessing_support) {
    OnInitializationResponse();
  }
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

  ring_buffer_map_.clear();
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
    case fuchsia_audio_device::DeviceType::kComposite:
      return has_composite_properties() && has_health_state() && checked_for_signalprocessing() &&
             dai_format_sets_retrieved() && ring_buffer_format_sets_retrieved();
    case fuchsia_audio_device::DeviceType::kInput:
    case fuchsia_audio_device::DeviceType::kOutput:
      return has_stream_config_properties() && has_health_state() &&
             checked_for_signalprocessing() && ring_buffer_format_sets_retrieved() &&
             has_gain_state() && has_plug_state();
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
    case fuchsia_audio_device::DeviceType::kComposite:
      ADR_LOG_METHOD(kLogDeviceInitializationProgress)
          << " (RECEIVED|pending)"                                                 //
          << "   " << (has_composite_properties() ? "PROPS" : "props")             //
          << "   " << (has_health_state() ? "HEALTH" : "health")                   //
          << "   " << (checked_for_signalprocessing() ? "SIGPROC" : "sigproc")     //
          << "   " << (dai_format_sets_retrieved() ? "DAIFORMATS" : "daiformats")  //
          << "   " << (ring_buffer_format_sets_retrieved() ? "RB_FORMATS" : "rb_formats");
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
    if (is_codec()) {
      if (codec_format_) {
        notify->DaiFormatChanged(fuchsia_audio_device::kDefaultDaiInterconnectElementId,
                                 codec_format_->dai_format, codec_format_->codec_format_info);
      } else {
        notify->DaiFormatChanged(fuchsia_audio_device::kDefaultDaiInterconnectElementId,
                                 std::nullopt, std::nullopt);
      }
      // Codec start state
      codec_start_state_.started ? notify->CodecStarted(codec_start_state_.start_stop_time)
                                 : notify->CodecStopped(codec_start_state_.start_stop_time);
    } else if (is_composite()) {
      for (auto [element_id, dai_format] : composite_dai_formats_) {
        notify->DaiFormatChanged(element_id, dai_format, std::nullopt);
      }
    }
  }

  LogObjectCounts();
  return true;
}

bool Device::DropControl() {
  ADR_LOG_METHOD(kLogDeviceMethods || kLogNotifyMethods);
  FX_CHECK(state_ != State::DeviceInitializing);

  // TODO drop ring buffers?

  auto control_notify = GetControlNotify();
  if (!control_notify) {
    ADR_LOG_METHOD(kLogNotifyMethods) << "already not controlled";
    return false;
  }

  control_notify_.reset();
  // We don't remove our ControlNotify from the observer list: we wait for it to self-invalidate.

  return true;
}

bool Device::AddObserver(const std::shared_ptr<ObserverNotify>& observer_to_add) {
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

  // For this new observer, "catch it up" on any current state that we already know.
  // This may include:
  //
  // (1) current signalprocessing topology.
  if (current_topology_id_.has_value()) {
    observer_to_add->TopologyChanged(*current_topology_id_);
  }

  // (2) ElementState, for each signalprocessing element where this has already been retrieved.
  for (const auto& element_record : sig_proc_element_map_) {
    if (element_record.second.state.has_value()) {
      observer_to_add->ElementStateChanged(element_record.first, *element_record.second.state);
    }
  }

  // (3) GainState (if StreamConfig).
  if (is_stream_config()) {
    observer_to_add->GainStateChanged({{
        .gain_db = *gain_state_->gain_db(),
        .muted = gain_state_->muted().value_or(false),
        .agc_enabled = gain_state_->agc_enabled().value_or(false),
    }});
  }

  // (4) PlugState (if Codec or StreamConfig).
  if (is_codec() || is_stream_config()) {
    observer_to_add->PlugStateChanged(*plug_state_->plugged()
                                          ? fuchsia_audio_device::PlugState::kPlugged
                                          : fuchsia_audio_device::PlugState::kUnplugged,
                                      zx::time(*plug_state_->plug_state_time()));
  }

  LogObjectCounts();

  // SetState(State::DeviceInitialized);
  return true;
}

// The Device dropped the driver RingBuffer FIDL. Notify any clients.
void Device::DeviceDroppedRingBuffer(ElementId element_id) {
  ADR_LOG_METHOD(kLogDeviceState);

  // This is distinct from DropRingBuffer in case we must notify our RingBuffer (via our Control).
  // We do so if we have 1) a Control and 2) a driver_format_ (thus a client-configured RingBuffer).
  if (auto notify = GetControlNotify(); notify) {
    if (auto rb_pair = ring_buffer_map_.find(element_id);
        rb_pair != ring_buffer_map_.end() && rb_pair->second.driver_format.has_value())
      notify->DeviceDroppedRingBuffer(element_id);
  }
  DropRingBuffer(element_id);
}

// Whether client- or device-originated, reset any state associated with an active RingBuffer.
void Device::DropRingBuffer(ElementId element_id) {
  ADR_LOG_METHOD(kLogDeviceState);

  // if (state_ != State::Error) {
  //   SetState(State::DeviceInitialized);
  // }

  auto rb_pair = ring_buffer_map_.find(element_id);
  if (rb_pair == ring_buffer_map_.end()) {
    return;
  }

  // If we've already cleaned out any state with the underlying driver RingBuffer, then we're done.
  auto& rb_record = rb_pair->second;
  if (!rb_record.ring_buffer_client.has_value() || !rb_record.ring_buffer_client->is_valid()) {
    return;
  }

  // Revert all configuration state related to the ring buffer.
  //
  rb_record.start_time.reset();  // Pause, if we are started.

  rb_record.requested_ring_buffer_bytes
      .reset();  // User must call CreateRingBuffer again, leading to ...
  rb_record.create_ring_buffer_callback = nullptr;

  rb_record.driver_format.reset();  // ... our re-calling ConnectToRingBufferFidl ...
  rb_record.vmo_format = {};
  rb_record.num_ring_buffer_frames.reset();  // ... and GetVmo ...
  rb_record.ring_buffer_vmo.reset();

  rb_record.ring_buffer_properties.reset();  // ... and GetProperties ...

  rb_record.delay_info.reset();   // ... and WatchDelayInfo ...
  rb_record.bytes_per_frame = 0;  // (.. which calls CalculateRequiredRingBufferSizes ...)
  rb_record.requested_ring_buffer_frames = 0u;
  rb_record.ring_buffer_producer_bytes = 0;
  rb_record.ring_buffer_consumer_bytes = 0;

  rb_record.active_channels_bitmask = 0;  // ... and SetActiveChannels.
  rb_record.active_channels_set_time.reset();

  // Clear our FIDL connection to the driver RingBuffer.
  (void)rb_record.ring_buffer_client->UnbindMaybeGetEndpoint();
  rb_record.ring_buffer_client.reset();
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

void Device::SetRingBufferState(ElementId element_id, RingBufferState ring_buffer_state) {
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; ignoring this";
    return;
  }

  auto rb_pair = ring_buffer_map_.find(element_id);
  if (rb_pair == ring_buffer_map_.end()) {
    ADR_WARN_METHOD() << "couldn't find RingBuffer for element_id " << element_id;
    return;
  }

  ADR_LOG_METHOD(kLogRingBufferState) << ring_buffer_state;
  rb_pair->second.ring_buffer_state = ring_buffer_state;
}

void Device::Initialize() {
  ADR_LOG_METHOD(kLogDeviceMethods);

  if (is_codec()) {
    RetrieveCodecProperties();
    RetrieveCodecHealthState();
    RetrieveSignalProcessingState();
    RetrieveCodecDaiFormatSets();
    RetrieveCodecPlugState();
  } else if (is_composite()) {
    RetrieveCompositeProperties();
    RetrieveCompositeHealthState();
    RetrieveSignalProcessingState();  // On completion, this starts format-set-retrieval.
  } else if (is_stream_config()) {
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
        ADR_LOG_METHOD(kLogCodecFidlResponses || kLogCompositeFidlResponses ||
                       kLogStreamConfigFidlResponses)
            << debug_context << ": will take no action on " << result.error_value();
      } else {
        FX_LOGS(ERROR) << debug_context << " failed: " << result.error_value() << ")";
        OnError(result.error_value().framework_error().status());
      }
    } else {
      FX_LOGS(ERROR) << debug_context << " failed: " << result.error_value() << ")";
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
      ADR_LOG_METHOD(kLogCodecFidlResponses || kLogCompositeFidlResponses ||
                     kLogStreamConfigFidlResponses)
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
        if (!ValidateStreamProperties(result->properties())) {
          OnError(ZX_ERR_INVALID_ARGS);
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
        if (!ValidateCodecProperties(result->properties())) {
          OnError(ZX_ERR_INVALID_ARGS);
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

void Device::RetrieveCompositeProperties() {
  ADR_LOG_METHOD(kLogCompositeFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; ignoring this";
    return;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts

  (*composite_client_)
      ->GetProperties()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Composite::GetProperties>& result) {
        if (LogResultFrameworkError(result, "GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogCompositeFidlResponses) << "Composite/GetProperties: success";
        if (!ValidateCompositeProperties(result->properties())) {
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        FX_CHECK(!has_composite_properties())
            << "Composite/GetProperties response: composite_properties_ already set";
        composite_properties_ = result->properties();
        SanitizeCompositePropertiesStrings(composite_properties_);
        // We have our clock domain now. Create the device clock.
        CreateDeviceClock();

        OnInitializationResponse();
      });
}

void Device::SanitizeCompositePropertiesStrings(
    std::optional<fuchsia_hardware_audio::CompositeProperties>& composite_properties) {
  if (!composite_properties) {
    FX_LOGS(ERROR) << __func__ << " called with unspecified CompositeProperties";
    return;
  }

  if (composite_properties->manufacturer()) {
    composite_properties->manufacturer(composite_properties->manufacturer()->substr(
        0, std::min<uint64_t>(composite_properties->manufacturer()->find('\0'),
                              fuchsia_hardware_audio::kMaxUiStringSize - 1)));
  }
  if (composite_properties->product()) {
    composite_properties->product(composite_properties->product()->substr(
        0, std::min<uint64_t>(composite_properties->product()->find('\0'),
                              fuchsia_hardware_audio::kMaxUiStringSize - 1)));
  }
}

void Device::RetrieveStreamRingBufferFormatSets() {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls);

  RetrieveRingBufferFormatSets(
      fuchsia_audio_device::kDefaultRingBufferElementId,
      [this](ElementId element_id,
             const std::vector<fuchsia_hardware_audio::SupportedFormats>& ring_buffer_format_sets) {
        if (state_ == State::Error) {
          ADR_WARN_OBJECT() << "device has an error while retrieving initial RingBuffer formats";
          return;
        }

        // Required for StreamConfig and Dai, absent for Codec and Composite.
        auto translated_ring_buffer_format_sets =
            TranslateRingBufferFormatSets(ring_buffer_format_sets);
        if (translated_ring_buffer_format_sets.empty()) {
          ADR_WARN_OBJECT() << "RingBuffer format sets could not be translated";
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        };
        element_driver_ring_buffer_format_sets_ = {{
            {
                element_id,
                ring_buffer_format_sets,
            },
        }};
        element_ring_buffer_format_sets_ = {{
            fuchsia_audio_device::ElementRingBufferFormatSet{{
                .element_id = element_id,
                .format_sets = translated_ring_buffer_format_sets,
            }},
        }};
        ring_buffer_format_sets_retrieved_ = true;
        OnInitializationResponse();
      });
}

void Device::RetrieveRingBufferFormatSets(
    ElementId element_id,
    fit::callback<void(ElementId, const std::vector<fuchsia_hardware_audio::SupportedFormats>&)>
        ring_buffer_format_sets_callback) {
  ADR_LOG_METHOD(kLogStreamConfigFidlCalls || kLogRingBufferMethods);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/42064765): handle command timeouts

  if (element_id != fuchsia_audio_device::kDefaultRingBufferElementId) {
    OnError(ZX_ERR_INVALID_ARGS);
    return;
  }
  ring_buffer_endpoint_ids_.insert(fuchsia_audio_device::kDefaultRingBufferElementId);

  (*stream_config_client_)
      ->GetSupportedFormats()
      .Then([this, element_id, rb_formats_callback = std::move(ring_buffer_format_sets_callback)](
                fidl::Result<fuchsia_hardware_audio::StreamConfig::GetSupportedFormats>&
                    result) mutable {
        if (LogResultFrameworkError(result, "GetSupportedFormats response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogStreamConfigFidlResponses)
            << "StreamConfig/GetSupportedFormats: success";
        if (!ValidateRingBufferFormatSets(result->supported_formats())) {
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        rb_formats_callback(element_id, result->supported_formats());
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

  if (is_codec()) {
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
  } else if (is_composite()) {
    ADR_LOG_METHOD(kLogCompositeFidlCalls) << "calling SignalProcessingConnect";
    auto status = (*composite_client_)->SignalProcessingConnect(std::move(sig_proc_server_end));

    if (status.is_error()) {
      if (status.error_value().is_canceled()) {
        // These indicate that we are already shutting down, so they aren't error conditions.
        ADR_LOG_METHOD(kLogCompositeFidlResponses)
            << "SignalProcessingConnect response will take no action on error "
            << status.error_value().FormatDescription();
        return;
      }

      FX_PLOGS(ERROR, status.error_value().status()) << __func__ << " returned error:";
      OnError(status.error_value().status());
      return;
    }
  } else if (is_stream_config()) {
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
            if (!ValidateElements(result->processing_elements())) {
              OnError(ZX_ERR_INVALID_ARGS);
              return;
            }

            sig_proc_elements_ = result->processing_elements();
            sig_proc_element_map_ = MapElements(sig_proc_elements_);
            if (sig_proc_element_map_.empty()) {
              ADR_WARN_OBJECT() << "Empty element map";
              OnError(ZX_ERR_INVALID_ARGS);
              return;
            }
            RetrieveSignalProcessingTopologies();

            if (is_composite()) {
              RetrieveCompositeDaiFormatSets();
              RetrieveCompositeRingBufferFormatSets();
            }
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
        if (!ValidateTopologies(result->topologies(), sig_proc_element_map_)) {
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        sig_proc_topologies_ = result->topologies();
        sig_proc_topology_map_ = MapTopologies(sig_proc_topologies_);
        if (sig_proc_topology_map_.empty()) {
          ADR_WARN_OBJECT() << "Empty topology map";
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

        TopologyId new_topology_id = result->topology_id();
        auto match = sig_proc_topology_map_.find(new_topology_id);
        if (match == sig_proc_topology_map_.end()) {
          ADR_WARN_OBJECT() << context << ": topology_id " << new_topology_id << " not found";
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        // Save the topology and notify Observers, but only if this is a change in topology.
        if (!current_topology_id_.has_value() || *current_topology_id_ != new_topology_id) {
          current_topology_id_ = new_topology_id;
          ForEachObserver([new_topology_id](auto obs) { obs->TopologyChanged(new_topology_id); });
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
    const auto& element = element_pair.second.element;
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
          if (!ValidateElementState(element_state, element)) {
            OnError(ZX_ERR_INVALID_ARGS);
            return;
          }
          ADR_LOG_OBJECT(kLogSignalProcessingFidlResponses) << context;

          // Save the state and notify Observers, but only if this is a change in element state.
          auto element_record = sig_proc_element_map_.find(*element.id());
          if (!element_record->second.state.has_value() ||
              *element_record->second.state != element_state) {
            element_record->second.state = element_state;
            // Notify any Observers of this change in element state.
            ForEachObserver([element_id = *element.id(), element_state](auto obs) {
              obs->ElementStateChanged(element_id, element_state);
            });
          }
        });
  }
}

bool Device::SetTopology(uint64_t topology_id) {
  ADR_LOG_METHOD(kLogSignalProcessingFidlCalls);
  if (state_ == State::Error || !supports_signalprocessing()) {
    return false;
  }
  if (!supports_signalprocessing()) {
    return false;
  }
  FX_DCHECK(sig_proc_client_->is_valid());

  if (sig_proc_topology_map_.find(topology_id) == sig_proc_topology_map_.end()) {
    ADR_WARN_METHOD() << "invalid topology_id";
    return false;
  }

  (*sig_proc_client_)
      ->SetTopology(topology_id)
      .Then(
          [this](
              fidl::Result<fuchsia_hardware_audio_signalprocessing::SignalProcessing::SetTopology>&
                  result) {
            if (LogResultError(result, "SigProc::SetTopology response")) {
              return;
            }
            ADR_LOG_OBJECT(kLogSignalProcessingFidlResponses) << "SigProc::SetTopology response";
            // We can expect our WatchTopology call to complete now.
          });

  return true;
}

void Device::RetrieveCodecDaiFormatSets() {
  ADR_LOG_METHOD(kLogCodecFidlCalls);
  RetrieveDaiFormatSets(
      fuchsia_audio_device::kDefaultDaiInterconnectElementId,
      [this](ElementId element_id,
             const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
        if (state_ == State::Error) {
          ADR_WARN_OBJECT() << "device already has an error";
          return;
        }
        element_dai_format_sets_.push_back({{element_id, dai_format_sets}});
        dai_format_sets_retrieved_ = true;
        OnInitializationResponse();
      });
}

// We have our signalprocessing Elements now; GetDaiFormats on each DAI Endpoint.
void Device::RetrieveCompositeDaiFormatSets() {
  ADR_LOG_METHOD(kLogCompositeFidlCalls);
  dai_endpoint_ids_ = dai_endpoints(sig_proc_element_map_);
  temp_dai_endpoint_ids_ = dai_endpoint_ids_;
  element_dai_format_sets_.clear();
  for (auto element_id : temp_dai_endpoint_ids_) {
    RetrieveDaiFormatSets(
        element_id,
        [this](ElementId element_id,
               const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
          element_dai_format_sets_.push_back({{element_id, dai_format_sets}});
          temp_dai_endpoint_ids_.erase(element_id);
          if (temp_dai_endpoint_ids_.empty()) {
            dai_format_sets_retrieved_ = true;
            OnInitializationResponse();
          }
        });
  }
}

void Device::RetrieveDaiFormatSets(
    ElementId element_id,
    fit::callback<void(ElementId, const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>&)>
        dai_format_sets_callback) {
  ADR_LOG_METHOD(kLogCodecFidlCalls || kLogCompositeFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }
  // TODO(https://fxbug.dev/113429): handle command timeouts

  if (is_codec()) {
    if (element_id != fuchsia_audio_device::kDefaultDaiInterconnectElementId) {
      OnError(ZX_ERR_INVALID_ARGS);
      return;
    }

    (*codec_client_)
        ->GetDaiFormats()
        .Then([this, element_id, callback = std::move(dai_format_sets_callback)](
                  fidl::Result<fuchsia_hardware_audio::Codec::GetDaiFormats>& result) mutable {
          if (LogResultError(result, "GetDaiFormats response")) {
            return;
          }

          ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/GetDaiFormats: success";

          if (!ValidateDaiFormatSets(result->formats())) {
            OnError(ZX_ERR_INVALID_ARGS);
            return;
          }
          callback(element_id, result->formats());
        });
  } else if (is_composite()) {
    ADR_LOG_METHOD(kLogCompositeFidlCalls) << " GetDaiFormats (element " << element_id << ")";
    (*composite_client_)
        ->GetDaiFormats(element_id)
        .Then([this, element_id, callback = std::move(dai_format_sets_callback)](
                  fidl::Result<fuchsia_hardware_audio::Composite::GetDaiFormats>& result) mutable {
          std::string str{"GetDaiFormats (element "};
          str.append(std::to_string(element_id)).append(") response");
          ADR_LOG_OBJECT(kLogCompositeFidlResponses) << str;
          if (state_ == State::Error) {
            ADR_WARN_OBJECT() << "device already has error during " << str;
            return;
          }
          if (LogResultError(result, str.c_str())) {
            return;
          }
          if (!ValidateDaiFormatSets(result->dai_formats())) {
            OnError(ZX_ERR_INVALID_ARGS);
            return;
          }

          callback(element_id, result->dai_formats());
        });
  }
}

// We have our signalprocessing Elements now; GetDaiFormats on each RingBuffer Endpoint.
void Device::RetrieveCompositeRingBufferFormatSets() {
  ADR_LOG_METHOD(kLogCompositeFidlCalls);
  ring_buffer_endpoint_ids_ = ring_buffer_endpoints(sig_proc_element_map_);
  temp_ring_buffer_endpoint_ids_ = ring_buffer_endpoint_ids_;

  element_ring_buffer_format_sets_.clear();
  for (auto id : ring_buffer_endpoint_ids_) {
    ADR_LOG_METHOD(kLogCompositeFidlCalls) << " GetRingBufferFormats (element " << id << ")";
    (*composite_client_)
        ->GetRingBufferFormats(id)
        .Then([this,
               id](fidl::Result<fuchsia_hardware_audio::Composite::GetRingBufferFormats>& result) {
          std::string str{"GetRingBufferFormats (element "};
          str.append(std::to_string(id)).append(") response");
          ADR_LOG_OBJECT(kLogCompositeFidlResponses) << str;
          if (state_ == State::Error) {
            ADR_WARN_OBJECT() << "device already has error during " << str;
            return;
          }
          if (LogResultError(result, str.c_str())) {
            return;
          }
          if (!ValidateRingBufferFormatSets(result->ring_buffer_formats())) {
            OnError(ZX_ERR_INVALID_ARGS);
            return;
          }
          auto translated_ring_buffer_format_sets =
              TranslateRingBufferFormatSets(result->ring_buffer_formats());
          if (translated_ring_buffer_format_sets.empty()) {
            ADR_WARN_OBJECT() << "Failed to translate " << str;
            OnError(ZX_ERR_INVALID_ARGS);
            return;
          }

          element_driver_ring_buffer_format_sets_.emplace_back(id, result->ring_buffer_formats());
          element_ring_buffer_format_sets_.push_back({{id, translated_ring_buffer_format_sets}});
          temp_ring_buffer_endpoint_ids_.erase(id);
          if (temp_ring_buffer_endpoint_ids_.empty()) {
            ring_buffer_format_sets_retrieved_ = true;
            OnInitializationResponse();
          }
        });
  }
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
        if (!ValidateGainState(result->gain_state())) {
          OnError(ZX_ERR_INVALID_ARGS);
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
        if (!ValidatePlugState(result->plug_state(), plug_detect_capabilities)) {
          OnError(ZX_ERR_INVALID_ARGS);
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
        if (!ValidatePlugState(result->plug_state(), plug_detect_capabilities)) {
          OnError(ZX_ERR_INVALID_ARGS);
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

void Device::RetrieveCompositeHealthState() {
  ADR_LOG_METHOD(kLogCompositeFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error; ignoring this";
    return;
  }

  // TODO(https://fxbug.dev/113429): handle command timeouts, because that's the most likely
  // indicator of an unhealthy driver/device.

  (*composite_client_)
      ->GetHealthState()
      .Then([this](fidl::Result<fuchsia_hardware_audio::Composite::GetHealthState>& result) {
        if (LogResultFrameworkError(result, "HealthState response")) {
          return;
        }

        auto old_health_state = health_state_;

        // An empty health state is permitted; it still indicates that the driver is responsive.
        health_state_ = result->state().healthy().value_or(true);
        // ...but if the driver actually self-reported as unhealthy, this is a problem.
        if (!*health_state_) {
          FX_LOGS(WARNING)
              << "RetrieveCompositeHealthState response: .healthy is FALSE (unhealthy)";
          OnError(ZX_ERR_IO);
          return;
        }

        ADR_LOG_OBJECT(kLogCompositeFidlResponses)
            << "RetrieveCompositeHealthState response: healthy";
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
  if (is_codec()) {
    // Optional for all device types.
    info.manufacturer(codec_properties_->manufacturer())
        .product(codec_properties_->product())
        // Required for Dai and StreamConfig; optional for Codec:
        .is_input(codec_properties_->is_input())
        // Required for Codec and Dai; optional for Composite; absent for StreamConfig:
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
  } else if (is_composite()) {
    // Optional for all device types:
    info.manufacturer(composite_properties_->manufacturer())
        .product(composite_properties_->product())
        .unique_instance_id(composite_properties_->unique_id())
        // Required for Dai and StreamConfig; optional for Composite; absent for Codec:
        .ring_buffer_format_sets(element_ring_buffer_format_sets_)
        // Required for Codec and Dai; optional for Composite; absent for StreamConfig:
        .dai_format_sets(element_dai_format_sets_)
        // Required for Composite, Dai and StreamConfig; absent for Codec:
        .clock_domain(composite_properties_->clock_domain());
  } else if (is_stream_config()) {
    // Optional for all device types:
    info.manufacturer(stream_config_properties_->manufacturer())
        .product(stream_config_properties_->product())
        .unique_instance_id(stream_config_properties_->unique_id())
        // Required for Dai and StreamConfig; optional for Codec; absent for Composite:
        .is_input(stream_config_properties_->is_input())
        // Required for Dai and StreamConfig; optional for Composite; absent for Codec:
        .ring_buffer_format_sets(ring_buffer_format_sets())
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

  if (!sig_proc_element_map_.empty()) {
    LogElementMap(sig_proc_element_map_);
  }

  (void)ValidateDeviceInfo(*device_info_);
}

void Device::CreateDeviceClock() {
  ADR_LOG_METHOD(kLogDeviceMethods);

  ClockDomain clock_domain;
  if (is_stream_config()) {
    FX_CHECK(stream_config_properties_->clock_domain()) << "Clock domain is required";
    clock_domain = stream_config_properties_->clock_domain().value_or(
        fuchsia_hardware_audio::kClockDomainMonotonic);
  } else if (is_composite()) {
    FX_CHECK(composite_properties_->clock_domain()) << "Clock domain is required";
    clock_domain = composite_properties_->clock_domain().value_or(
        fuchsia_hardware_audio::kClockDomainMonotonic);
  } else {
    ADR_WARN_METHOD() << "Cannot create a device clock for device_type " << device_type();
    return;
  }

  device_clock_ = RealClock::CreateFromMonotonic(
      "'" + name_ + "' device clock", clock_domain,
      (clock_domain != fuchsia_hardware_audio::kClockDomainMonotonic));
}

// Create a duplicate handle to our clock with limited rights. We can transfer it to a client who
// can only read and duplicate. Specifically, they cannot change this clock's rate or offset.
zx::result<zx::clock> Device::GetReadOnlyClock() const {
  ADR_LOG_METHOD(kLogDeviceMethods);

  if (!device_clock_) {
    // This device type does not expose a clock.
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  auto dupe_clock = device_clock_->DuplicateZxClockReadOnly();
  if (!dupe_clock) {
    return zx::error(ZX_ERR_ACCESS_DENIED);
  }

  return zx::ok(std::move(*dupe_clock));
}

// Determine the full fuchsia_hardware_audio::Format needed for ConnectRingBufferFidl.
// This method expects that the required fields are present.
std::optional<fuchsia_hardware_audio::Format> Device::SupportedDriverFormatForClientFormat(
    ElementId element_id, const fuchsia_audio::Format& client_format) {
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

  std::vector<fuchsia_hardware_audio::SupportedFormats> driver_ring_buffer_format_sets;
  for (const auto& element_entry_pair : element_driver_ring_buffer_format_sets_) {
    if (element_entry_pair.first == element_id) {
      driver_ring_buffer_format_sets = element_entry_pair.second;
    }
  }
  if (driver_ring_buffer_format_sets.empty()) {
    FX_LOGS(WARNING) << __func__ << ": no driver ring_buffer_format_set found for element_id "
                     << element_id;
    return {};
  }
  // If format/bytes/rate/channels all match, save the highest valid_bits within our limit.
  uint8_t best_valid_bits = 0;
  for (const auto& ring_buffer_format_set : driver_ring_buffer_format_sets) {
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

void Device::SetDaiFormat(ElementId element_id,
                          const fuchsia_hardware_audio::DaiFormat& dai_format) {
  ADR_LOG_METHOD(kLogCodecFidlCalls || kLogCompositeFidlCalls);

  auto notify = GetControlNotify();
  if (!notify) {
    ADR_WARN_METHOD() << "Device must be controlled before SetDaiFormat can be called";
    return;
  }

  if (!is_codec() && !is_composite()) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot SetDaiFormat";
    notify->DaiFormatNotSet(element_id, dai_format,
                            fuchsia_audio_device::ControlSetDaiFormatError::kWrongDeviceType);
    return;
  }

  FX_CHECK(state_ != State::DeviceInitializing)
      << "Cannot call SetDaiFormat before device is initialized";
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    notify->DaiFormatNotSet(element_id, dai_format,
                            fuchsia_audio_device::ControlSetDaiFormatError::kDeviceError);
    return;
  }

  if (is_codec() ? (element_id != fuchsia_audio_device::kDefaultDaiInterconnectElementId)
                 : (dai_endpoint_ids_.find(element_id) == dai_endpoint_ids_.end())) {
    ADR_WARN_METHOD() << "element_id not found, or not a DaiInterconnect Endpoint";
    notify->DaiFormatNotSet(element_id, dai_format,
                            fuchsia_audio_device::ControlSetDaiFormatError::kInvalidElementId);
    return;
  }

  if (!ValidateDaiFormat(dai_format)) {
    ADR_WARN_METHOD() << "Invalid dai_format -- cannot SetDaiFormat";
    notify->DaiFormatNotSet(element_id, dai_format,
                            fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat);
    return;
  }

  if (!DaiFormatIsSupported(element_id, dai_format_sets(), dai_format)) {
    ADR_WARN_METHOD() << "Unsupported dai_format cannot be set";
    notify->DaiFormatNotSet(element_id, dai_format,
                            fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch);
    return;
  }

  // Check for no-change

  if (is_codec()) {
    // auto new_dai_format = dai_format;

    // TODO(https://fxbug.dev/113429): handle command timeouts

    (*codec_client_)
        ->SetDaiFormat(dai_format)
        .Then([this, element_id,
               dai_format](fidl::Result<fuchsia_hardware_audio::Codec::SetDaiFormat>& result) {
          auto notify = GetControlNotify();
          if (!notify) {
            ADR_WARN_OBJECT()
                << "SetDaiFormat response: device must be controlled before SetDaiFormat can be called";
            return;
          }

          if (state_ == State::Error) {
            ADR_WARN_OBJECT() << "Codec/SetDaiFormat response: device already has an error";
            notify->DaiFormatNotSet(element_id, dai_format,
                                    fuchsia_audio_device::ControlSetDaiFormatError::kDeviceError);
            return;
          }

          if (!result.is_ok()) {
            fuchsia_audio_device::ControlSetDaiFormatError error;
            // These types of errors don't lead us to mark the device as in Error state.
            if (result.error_value().is_domain_error() &&
                (result.error_value().domain_error() == ZX_ERR_INVALID_ARGS ||
                 result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED)) {
              ADR_WARN_OBJECT()
                  << "Codec/SetDaiFormat response: ZX_ERR_INVALID_ARGS or ZX_ERR_NOT_SUPPORTED";
              error = (result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED
                           ? fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch
                           : fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat);
            } else {
              LogResultError(result, "SetDaiFormat response");
              error = fuchsia_audio_device::ControlSetDaiFormatError::kOther;
            }
            notify->DaiFormatNotSet(element_id, dai_format, error);
            return;
          }

          ADR_LOG_OBJECT(kLogCodecFidlResponses) << "Codec/SetDaiFormat: success";
          if (!ValidateCodecFormatInfo(result->state())) {
            FX_LOGS(ERROR) << "Codec/SetDaiFormat error: " << result.error_value();
            OnError(ZX_ERR_INVALID_ARGS);
            notify->DaiFormatNotSet(
                element_id, dai_format,
                fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat);
            return;
          }

          if (codec_format_ && codec_format_->dai_format == dai_format &&
              codec_format_->codec_format_info == result->state()) {
            // No DaiFormat change for this element
            notify->DaiFormatNotSet(element_id, dai_format,
                                    fuchsia_audio_device::ControlSetDaiFormatError(0));
            return;
          }

          // Reset Start state and DaiFormat (if it's a change). Notify our controlling entity.
          bool was_started = codec_start_state_.started;
          if (was_started) {
            codec_start_state_.started = false;
            codec_start_state_ = CodecStartState{false, zx::clock::get_monotonic()};
          }
          codec_format_ = CodecFormat{dai_format, result->state()};
          if (was_started) {
            notify->CodecStopped(codec_start_state_.start_stop_time);
          }
          notify->DaiFormatChanged(element_id, codec_format_->dai_format,
                                   codec_format_->codec_format_info);
        });
  } else {
    // TODO(https://fxbug.dev/113429): handle command timeouts

    (*composite_client_)
        ->SetDaiFormat({element_id, dai_format})
        .Then([this, element_id,
               dai_format](fidl::Result<fuchsia_hardware_audio::Composite::SetDaiFormat>& result) {
          std::string context("Composite/SetDaiFormat response: ");
          auto notify = GetControlNotify();
          if (!notify) {
            ADR_WARN_OBJECT() << context
                              << "device must be controlled before SetDaiFormat can be called";
            return;
          }

          if (state_ == State::Error) {
            ADR_WARN_OBJECT() << context << "device already has an error";
            notify->DaiFormatNotSet(element_id, dai_format,
                                    fuchsia_audio_device::ControlSetDaiFormatError::kDeviceError);
            return;
          }

          if (!result.is_ok()) {
            fuchsia_audio_device::ControlSetDaiFormatError error;
            // These types of errors don't lead us to mark the device as in Error state.
            if (result.error_value().is_domain_error() &&
                (result.error_value().domain_error() ==
                 fuchsia_hardware_audio::DriverError::kInvalidArgs)) {
              ADR_WARN_OBJECT() << context << "kInvalidArgs";
              error = fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat;
            } else if (result.error_value().is_domain_error() &&
                       result.error_value().domain_error() ==
                           fuchsia_hardware_audio::DriverError::kNotSupported) {
              ADR_WARN_OBJECT() << context << "kNotSupported";
              error = fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch;
            } else {
              LogResultError(result, context.c_str());
              error = fuchsia_audio_device::ControlSetDaiFormatError::kOther;
            }
            notify->DaiFormatNotSet(element_id, dai_format, error);
            return;
          }

          ADR_LOG_OBJECT(kLogCompositeFidlResponses) << "Composite/SetDaiFormat: success";
          if (auto match = composite_dai_formats_.find(element_id);
              match != composite_dai_formats_.end() && match->second == dai_format) {
            // No DaiFormat change for this element
            notify->DaiFormatNotSet(element_id, dai_format,
                                    fuchsia_audio_device::ControlSetDaiFormatError(0));
            return;
          }

          // Unlike Codec, Composite DAI elements don't generate `CodecStopped` notifications.
          composite_dai_formats_.insert_or_assign(element_id, dai_format);
          notify->DaiFormatChanged(element_id, dai_format, std::nullopt);
        });
  }
}

bool Device::Reset() {
  if (!is_codec() && !is_composite()) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot Reset";
    return false;
  }

  ADR_LOG_METHOD(kLogCodecFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Codec must be controlled before Reset can be called";
    return false;
  }

  // TODO(https://fxbug.dev/113429): handle command timeouts

  if (is_codec()) {
    (*codec_client_)
        ->Reset()
        .Then([this](fidl::Result<fuchsia_hardware_audio::Codec::Reset>& result) {
          std::string context = "Codec/Reset response";
          if (LogResultFrameworkError(result, context.c_str())) {
            return;
          }
          ADR_LOG_OBJECT(kLogCodecFidlResponses) << context;

          // Reset to Stopped (if Started), even if no ControlNotify listens for notifications.
          if (codec_start_state_.started) {
            codec_start_state_.started = false;
            codec_start_state_.start_stop_time = zx::clock::get_monotonic();
            if (auto notify = GetControlNotify(); notify) {
              notify->CodecStopped(codec_start_state_.start_stop_time);
            }
          }

          if (codec_format_) {
            // Reset our DaiFormat, even if no ControlNotify listens for notifications.
            codec_format_.reset();
            if (auto notify = GetControlNotify(); notify) {
              notify->DaiFormatChanged(fuchsia_audio_device::kDefaultDaiInterconnectElementId,
                                       std::nullopt, std::nullopt);
            }
          }

          // TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology,
          // gain). When implemented, reset the signalprocessing topology and all elements, here.
        });
  }
  if (is_composite()) {
    (*composite_client_)
        ->Reset()
        .Then([this](fidl::Result<fuchsia_hardware_audio::Composite::Reset>& result) {
          std::string context = "Composite/Reset response";
          if (LogResultError(result, context.c_str())) {
            return;
          }
          ADR_LOG_OBJECT(kLogCompositeFidlResponses) << context;

          ////    Expect DeviceDroppedRingBuffer(element_id) from the driver, for all RingBuffers.
          ////    We shouldn't need to format-reset them or reset any hanging gets or expressly
          ////    change their state in any way.
          //
          // ring_buffer_map_.clear();

          // For each DAI node, clear its DaiFormat and notify our controlling entity.

          if (!composite_dai_formats_.empty()) {
            // Reset our DaiFormat, even if no ControlNotify listens for notifications.
            if (auto notify = GetControlNotify(); notify) {
              for (const auto& [element_id, _] : composite_dai_formats_) {
                notify->DaiFormatChanged(element_id, std::nullopt, std::nullopt);
              }
            }
            composite_dai_formats_.clear();
          }

          ////    Expect the driver's WatchTopology and WatchElementState hanging-gets to all fire.
          ////    We shouldn't need to touch the hardware in any way until then.
          //
          // TODO(https://fxbug.dev/323270827): implement signalprocessing
          // current_topology_id_.reset();
          // for (auto& [element_id, element_record] : sig_proc_element_map_) {
          //   element_record.state.reset();
          // }
          //
          ////    Maybe ObserverNotify should emit optionals for [Topology|ElementState]Changed?
          //
          // ForEachObserver([...](auto obs) { ... });
        });
  }

  return true;
}

bool Device::CodecStart() {
  if (!is_codec()) {
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
  if (!is_codec()) {
    ADR_WARN_METHOD() << "Incorrect device_type " << device_type_ << " cannot Stop";
    return false;
  }

  ADR_LOG_METHOD(kLogCodecFidlCalls);
  FX_CHECK(state_ != State::DeviceInitializing);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return false;
  }
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

  // TODO(https://fxbug.dev/113429): handle command timeouts

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

bool Device::CreateRingBuffer(
    ElementId element_id, const fuchsia_hardware_audio::Format& format,
    uint32_t requested_ring_buffer_bytes,
    fit::callback<void(
        fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>)>
        create_ring_buffer_callback) {
  ADR_LOG_METHOD(kLogRingBufferMethods);
  if (!is_composite() && !is_stream_config()) {
    ADR_WARN_METHOD() << "Wrong device type";
    create_ring_buffer_callback(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kWrongDeviceType));
    return false;
  }

  if (!GetControlNotify()) {
    ADR_WARN_METHOD() << "Device must be controlled before CreateRingBuffer can be called";
    create_ring_buffer_callback(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kOther));
    return false;
  }

  if (ring_buffer_endpoint_ids_.find(element_id) == ring_buffer_endpoint_ids_.end()) {
    create_ring_buffer_callback(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kInvalidElementId));
    return false;
  }

  ring_buffer_map_.erase(element_id);

  // This method create the ring_buffer map entry, upon success.
  if (auto status = ConnectRingBufferFidl(element_id, format);
      status != fuchsia_audio_device::ControlCreateRingBufferError(0)) {
    create_ring_buffer_callback(fit::error(status));
    return false;
  }

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  ring_buffer.requested_ring_buffer_bytes = requested_ring_buffer_bytes;
  ring_buffer.create_ring_buffer_callback = std::move(create_ring_buffer_callback);

  RetrieveRingBufferProperties(element_id);
  RetrieveDelayInfo(element_id);

  return true;
}

// Here, we detect all the error cases that we can, before calling into the driver. If we call into
// the driver, we return "no error", otherwise we return an error code that can be returned to
// clients as the reason the CreateRingBuffer call failed.
fuchsia_audio_device::ControlCreateRingBufferError Device::ConnectRingBufferFidl(
    ElementId element_id, fuchsia_hardware_audio::Format driver_format) {
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return fuchsia_audio_device::ControlCreateRingBufferError::kDeviceError;
  }

  if (!ValidateRingBufferFormat(driver_format)) {
    return fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat;
  }

  auto bytes_per_sample = driver_format.pcm_format()->bytes_per_sample();
  auto sample_format = driver_format.pcm_format()->sample_format();
  if (!ValidateSampleFormatCompatibility(bytes_per_sample, sample_format)) {
    return fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat;
  }

  if (!RingBufferFormatIsSupported(element_id, element_ring_buffer_format_sets_, driver_format)) {
    ADR_WARN_METHOD() << "RingBuffer not supported";
    return fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch;
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::RingBuffer>();
  if (!endpoints.is_ok()) {
    FX_PLOGS(ERROR, endpoints.status_value())
        << "CreateEndpoints<fuchsia_hardware_audio::RingBuffer> failed";
    OnError(endpoints.status_value());
    return fuchsia_audio_device::ControlCreateRingBufferError::kDeviceError;
  }

  if (is_stream_config()) {
    auto result =
        (*stream_config_client_)->CreateRingBuffer({driver_format, std::move(endpoints->server)});
    if (!result.is_ok()) {
      FX_PLOGS(ERROR, result.error_value().status()) << "StreamConfig/CreateRingBuffer failed";
      return fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch;
    }
  } else {
    (*composite_client_)
        ->CreateRingBuffer({element_id, driver_format, std::move(endpoints->server)})
        .Then([this](fidl::Result<fuchsia_hardware_audio::Composite::CreateRingBuffer>& result) {
          std::string context{"Composite/CreateRingBuffer response"};
          if (LogResultError(result, context.c_str())) {
            return;
          }
          ADR_LOG_OBJECT(kLogCompositeFidlResponses) << context;
        });
  }

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
  uint64_t active_channels_bitmask =
      (1ull << driver_format.pcm_format()->number_of_channels()) - 1ull;

  RingBufferRecord ring_buffer_record{
      .ring_buffer_state = RingBufferState::NotCreated,
      .ring_buffer_handler =
          std::make_unique<EndpointFidlErrorHandler<fuchsia_hardware_audio::RingBuffer>>(
              this, element_id, "RingBuffer"),
      .vmo_format = {{
          .sample_type = *sample_type,
          .channel_count = driver_format.pcm_format()->number_of_channels(),
          .frames_per_second = driver_format.pcm_format()->frame_rate(),
          // TODO(https://fxbug.dev/42168795): handle .channel_layout, when communicated from
          // driver.
      }},
      .driver_format = driver_format,
      .active_channels_bitmask = active_channels_bitmask,
  };
  ring_buffer_record.ring_buffer_client = fidl::Client<fuchsia_hardware_audio::RingBuffer>(
      std::move(endpoints->client), dispatcher_, ring_buffer_record.ring_buffer_handler.get()),

  ring_buffer_map_.insert_or_assign(element_id, std::move(ring_buffer_record));

  (void)SetActiveChannels(element_id, active_channels_bitmask, [this](zx::result<zx::time> result) {
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
  SetRingBufferState(element_id, RingBufferState::Creating);

  return fuchsia_audio_device::ControlCreateRingBufferError(0);
}

void Device::RetrieveRingBufferProperties(ElementId element_id) {
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.ring_buffer_client.has_value() &&
           ring_buffer.ring_buffer_client->is_valid());

  (*ring_buffer.ring_buffer_client)
      ->GetProperties()
      .Then([this, &ring_buffer,
             element_id](fidl::Result<fuchsia_hardware_audio::RingBuffer::GetProperties>& result) {
        if (LogResultFrameworkError(result, "RingBuffer/GetProperties response")) {
          return;
        }

        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/GetProperties: success";
        if (!ValidateRingBufferProperties(result->properties())) {
          FX_LOGS(ERROR) << "RingBuffer/GetProperties error: " << result.error_value();
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        ring_buffer.ring_buffer_properties = result->properties();
        CheckForRingBufferReady(element_id);
      });
}

void Device::RetrieveDelayInfo(ElementId element_id) {
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.ring_buffer_client.has_value() &&
           ring_buffer.ring_buffer_client->is_valid());

  (*ring_buffer.ring_buffer_client)
      ->WatchDelayInfo()
      .Then([this, &ring_buffer,
             element_id](fidl::Result<fuchsia_hardware_audio::RingBuffer::WatchDelayInfo>& result) {
        if (LogResultFrameworkError(result, "RingBuffer/WatchDelayInfo response")) {
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/WatchDelayInfo: success";

        if (!ValidateDelayInfo(result->delay_info())) {
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }

        ring_buffer.delay_info = result->delay_info();
        // If requested_ring_buffer_bytes_ is already set, but num_ring_buffer_frames_ isn't, then
        // we're getting delay info as part of creating a ring buffer. Otherwise,
        // requested_ring_buffer_bytes_ must be set separately before calling GetVmo.
        if (ring_buffer.requested_ring_buffer_bytes && !ring_buffer.num_ring_buffer_frames) {
          // Needed, to set requested_ring_buffer_frames_ before calling GetVmo.
          CalculateRequiredRingBufferSizes(element_id);

          FX_CHECK(device_info_->clock_domain());
          const auto clock_position_notifications_per_ring =
              *device_info_->clock_domain() == fuchsia_hardware_audio::kClockDomainMonotonic ? 0
                                                                                             : 2;
          GetVmo(element_id, static_cast<uint32_t>(ring_buffer.requested_ring_buffer_frames),
                 clock_position_notifications_per_ring);
        }

        // Notify our controlling entity, if we have one.
        if (auto notify = GetControlNotify(); notify) {
          notify->DelayInfoChanged(element_id,
                                   {{
                                       .internal_delay = ring_buffer.delay_info->internal_delay(),
                                       .external_delay = ring_buffer.delay_info->external_delay(),
                                   }});
        }
        RetrieveDelayInfo(element_id);
      });
}

void Device::GetVmo(ElementId element_id, uint32_t min_frames,
                    uint32_t position_notifications_per_ring) {
  ADR_LOG_METHOD(kLogRingBufferMethods || kLogRingBufferFidlCalls);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.ring_buffer_client.has_value() &&
           ring_buffer.ring_buffer_client->is_valid());
  FX_CHECK(ring_buffer.driver_format);

  (*ring_buffer.ring_buffer_client)
      ->GetVmo({{.min_frames = min_frames,
                 .clock_recovery_notifications_per_ring = position_notifications_per_ring}})
      .Then([this, &ring_buffer,
             element_id](fidl::Result<fuchsia_hardware_audio::RingBuffer::GetVmo>& result) {
        if (LogResultError(result, "RingBuffer/GetVmo response")) {
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/GetVmo: success";

        if (!ValidateRingBufferVmo(result->ring_buffer(), result->num_frames(),
                                   *ring_buffer.driver_format)) {
          FX_PLOGS(ERROR, ZX_ERR_INVALID_ARGS) << "Error in RingBuffer/GetVmo response";
          OnError(ZX_ERR_INVALID_ARGS);
          return;
        }
        ring_buffer.ring_buffer_vmo = std::move(result->ring_buffer());
        ring_buffer.num_ring_buffer_frames = result->num_frames();
        CheckForRingBufferReady(element_id);
      });
}

// RingBuffer FIDL successful-response handlers.
void Device::CheckForRingBufferReady(ElementId element_id) {
  ADR_LOG_METHOD(kLogRingBufferFidlResponses);
  if (state_ == State::Error) {
    ADR_WARN_METHOD() << "device already has an error";
    return;
  }

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  // Check whether we are tearing down, or conversely have already set up the ring buffer.
  if (ring_buffer.ring_buffer_state != RingBufferState::Creating) {
    return;
  }

  // We're creating the ring buffer but don't have all our prerequisites yet.
  if (!ring_buffer.ring_buffer_properties || !ring_buffer.delay_info ||
      !ring_buffer.num_ring_buffer_frames) {
    return;
  }

  auto ref_clock = GetReadOnlyClock();
  if (!ref_clock.is_ok()) {
    ADR_WARN_METHOD() << "reference clock is not ok";
    ring_buffer.create_ring_buffer_callback(
        fit::error(fuchsia_audio_device::ControlCreateRingBufferError::kDeviceError));
    ring_buffer.create_ring_buffer_callback = nullptr;
    OnError(ZX_ERR_INTERNAL);
    return;
  }

  SetRingBufferState(element_id, RingBufferState::Stopped);

  FX_CHECK(ring_buffer.create_ring_buffer_callback);
  ring_buffer.create_ring_buffer_callback(fit::success(Device::RingBufferInfo{
      .ring_buffer = fuchsia_audio::RingBuffer{{
          .buffer = fuchsia_mem::Buffer{{
              .vmo = std::move(ring_buffer.ring_buffer_vmo),
              .size = *ring_buffer.num_ring_buffer_frames * ring_buffer.bytes_per_frame,
          }},
          .format = ring_buffer.vmo_format,
          .producer_bytes = ring_buffer.ring_buffer_producer_bytes,
          .consumer_bytes = ring_buffer.ring_buffer_consumer_bytes,
          .reference_clock = std::move(*ref_clock),
          .reference_clock_domain = *device_info_->clock_domain(),
      }},
      .properties = fuchsia_audio_device::RingBufferProperties{{
          .valid_bits_per_sample = valid_bits_per_sample(element_id),
          .turn_on_delay = ring_buffer.ring_buffer_properties->turn_on_delay().value_or(0),
      }},
  }));
  ring_buffer.create_ring_buffer_callback = nullptr;
}

// Returns TRUE if it actually calls out to the driver. It avoid doing so if it already knows
// that this RingBuffer does not support SetActiveChannels.
bool Device::SetActiveChannels(
    ElementId element_id, uint64_t channel_bitmask,
    fit::callback<void(zx::result<zx::time>)> set_active_channels_callback) {
  ADR_LOG_METHOD(kLogRingBufferFidlCalls);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.ring_buffer_client.has_value() &&
           ring_buffer.ring_buffer_client->is_valid());

  // If we already know this device doesn't support SetActiveChannels, do nothing.
  if (!ring_buffer.supports_set_active_channels.value_or(true)) {
    return false;
  }
  (*ring_buffer.ring_buffer_client)
      ->SetActiveChannels({{.active_channels_bitmask = channel_bitmask}})
      .Then(
          [this, &ring_buffer, channel_bitmask, callback = std::move(set_active_channels_callback)](
              fidl::Result<fuchsia_hardware_audio::RingBuffer::SetActiveChannels>& result) mutable {
            if (result.is_error() && result.error_value().is_domain_error() &&
                result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED) {
              ADR_LOG_OBJECT(kLogRingBufferFidlResponses)
                  << "RingBuffer/SetActiveChannels: device does not support this method";
              ring_buffer.supports_set_active_channels = false;
              callback(zx::error(ZX_ERR_NOT_SUPPORTED));
              return;
            }
            if (LogResultError(result, "RingBuffer/SetActiveChannels response")) {
              ring_buffer.supports_set_active_channels = false;
              callback(zx::error(ZX_ERR_INTERNAL));
              return;
            }

            ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/SetActiveChannels: success";

            ring_buffer.supports_set_active_channels = true;
            ring_buffer.active_channels_bitmask = channel_bitmask;
            ring_buffer.active_channels_set_time = zx::time(result->set_time());
            callback(zx::ok(*ring_buffer.active_channels_set_time));
            LogActiveChannels(ring_buffer.active_channels_bitmask,
                              *ring_buffer.active_channels_set_time);
          });
  return true;
}

void Device::StartRingBuffer(ElementId element_id,
                             fit::callback<void(zx::result<zx::time>)> start_callback) {
  ADR_LOG_METHOD(kLogRingBufferFidlCalls);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.ring_buffer_client.has_value() &&
           ring_buffer.ring_buffer_client->is_valid());

  (*ring_buffer.ring_buffer_client)
      ->Start()
      .Then([this, &ring_buffer, element_id, callback = std::move(start_callback)](
                fidl::Result<fuchsia_hardware_audio::RingBuffer::Start>& result) mutable {
        if (LogResultFrameworkError(result, "RingBuffer/Start response")) {
          callback(zx::error(ZX_ERR_INTERNAL));
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/Start: success";

        ring_buffer.start_time = zx::time(result->start_time());
        callback(zx::ok(*ring_buffer.start_time));
        SetRingBufferState(element_id, RingBufferState::Started);
      });
}

void Device::StopRingBuffer(ElementId element_id, fit::callback<void(zx_status_t)> stop_callback) {
  ADR_LOG_METHOD(kLogRingBufferFidlCalls);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.ring_buffer_client.has_value() &&
           ring_buffer.ring_buffer_client->is_valid());

  (*ring_buffer.ring_buffer_client)
      ->Stop()
      .Then([this, &ring_buffer, element_id, callback = std::move(stop_callback)](
                fidl::Result<fuchsia_hardware_audio::RingBuffer::Stop>& result) mutable {
        if (LogResultFrameworkError(result, "RingBuffer/Stop response")) {
          callback(ZX_ERR_INTERNAL);
          return;
        }
        ADR_LOG_OBJECT(kLogRingBufferFidlResponses) << "RingBuffer/Stop: success";

        ring_buffer.start_time.reset();
        callback(ZX_OK);
        SetRingBufferState(element_id, RingBufferState::Stopped);
      });
}

// Uses the VMO format and ring buffer properties, to set bytes_per_frame,
// requested_ring_buffer_frames, ring_buffer_consumer_bytes and ring_buffer_producer_bytes.
void Device::CalculateRequiredRingBufferSizes(ElementId element_id) {
  ADR_LOG_METHOD(kLogRingBufferMethods);

  auto& ring_buffer = ring_buffer_map_.find(element_id)->second;
  FX_CHECK(ring_buffer.vmo_format.channel_count());
  FX_CHECK(ring_buffer.vmo_format.sample_type());
  FX_CHECK(ring_buffer.vmo_format.frames_per_second());
  FX_CHECK(ring_buffer.ring_buffer_properties);
  FX_CHECK(ring_buffer.ring_buffer_properties->driver_transfer_bytes());
  FX_CHECK(ring_buffer.requested_ring_buffer_bytes);
  FX_CHECK(*ring_buffer.requested_ring_buffer_bytes > 0);

  switch (*ring_buffer.vmo_format.sample_type()) {
    case fuchsia_audio::SampleType::kUint8:
      ring_buffer.bytes_per_frame = 1;
      break;
    case fuchsia_audio::SampleType::kInt16:
      ring_buffer.bytes_per_frame = 2;
      break;
    case fuchsia_audio::SampleType::kInt32:
    case fuchsia_audio::SampleType::kFloat32:
      ring_buffer.bytes_per_frame = 4;
      break;
    case fuchsia_audio::SampleType::kFloat64:
      ring_buffer.bytes_per_frame = 8;
      break;
    default:
      FX_LOGS(FATAL) << __func__ << ": unknown fuchsia_audio::SampleType";
      __UNREACHABLE;
  }
  ring_buffer.bytes_per_frame *= *ring_buffer.vmo_format.channel_count();

  ring_buffer.requested_ring_buffer_frames =
      media::TimelineRate{1, ring_buffer.bytes_per_frame}.Scale(
          *ring_buffer.requested_ring_buffer_bytes, media::TimelineRate::RoundingMode::Ceiling);

  // Determine whether the RingBuffer client is a Producer or a Consumer.
  // If the RingBuffer element is "outgoing" (if it is a source in Topology edges), then the client
  // indeed _produces_ the frames that populate the RingBuffer.
  bool element_is_outgoing = false, element_is_incoming = false;
  if (is_composite()) {
    FX_CHECK(current_topology_id_.has_value());
    auto topology_match = sig_proc_topology_map_.find(*current_topology_id_);
    FX_CHECK(topology_match != sig_proc_topology_map_.end());
    auto& topology = sig_proc_topology_map_.find(*current_topology_id_)->second;
    for (auto& edge : topology) {
      if (edge.processing_element_id_from() == element_id) {
        element_is_outgoing = true;
      }
      if (edge.processing_element_id_to() == element_id) {
        element_is_incoming = true;
      }
    }
    FX_CHECK(element_is_outgoing != element_is_incoming);
  } else if (is_stream_config_output()) {
    element_is_outgoing = true;
  }

  // We don't include driver transfer size in our VMO size request (requested_ring_buffer_frames_)
  // ... but we do communicate it in our description of ring buffer producer/consumer "zones".
  uint64_t driver_bytes = *ring_buffer.ring_buffer_properties->driver_transfer_bytes() +
                          ring_buffer.bytes_per_frame - 1;
  driver_bytes -= (driver_bytes % ring_buffer.bytes_per_frame);

  if (element_is_outgoing) {
    ring_buffer.ring_buffer_consumer_bytes = driver_bytes;
    ring_buffer.ring_buffer_producer_bytes =
        ring_buffer.requested_ring_buffer_frames * ring_buffer.bytes_per_frame;
  } else {
    ring_buffer.ring_buffer_producer_bytes = driver_bytes;
    ring_buffer.ring_buffer_consumer_bytes =
        ring_buffer.requested_ring_buffer_frames * ring_buffer.bytes_per_frame;
  }

  // TODO(https://fxbug.dev/42069012): validate this case; we don't surface an error to the caller.
  if (ring_buffer.requested_ring_buffer_frames > std::numeric_limits<uint32_t>::max()) {
    ADR_WARN_METHOD() << "requested_ring_buffer_frames cannot exceed uint32_t::max()";
    ring_buffer.requested_ring_buffer_frames = std::numeric_limits<uint32_t>::max();
  }
}

// TODO(https://fxbug.dev/42069013): implement, via RingBuffer/WatchClockRecoveryPositionInfo.
void Device::RecoverDeviceClockFromPositionInfo() { ADR_LOG_METHOD(kLogRingBufferMethods); }

// TODO(https://fxbug.dev/42069013): implement this.
void Device::StopDeviceClockRecovery() { ADR_LOG_METHOD(kLogRingBufferMethods); }

}  // namespace media_audio

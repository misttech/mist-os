// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>

#include <cstdint>
#include <memory>
#include <optional>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <utility>

#include "src/media/audio/lib/clock/clock.h"
#include "src/media/audio/services/common/vector_of_weak_ptr.h"
#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/inspector.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/observer_notify.h"

namespace media_audio {

class Device;

// This class represents a driver and audio device, once it is detected.
class Device : public std::enable_shared_from_this<Device> {
 public:
  static std::shared_ptr<Device> Create(std::weak_ptr<DevicePresenceWatcher> presence_watcher,
                                        async_dispatcher_t* dispatcher, std::string_view name,
                                        fuchsia_audio_device::DeviceType device_type,
                                        fuchsia_audio_device::DriverClient driver_client);
  ~Device();

  template <typename ProtocolT>
  class RingBufferFidlErrorHandler final : public fidl::AsyncEventHandler<ProtocolT> {
   public:
    RingBufferFidlErrorHandler(Device* device, ElementId element_id, std::string name)
        : device_(device), element_id_(element_id), name_(std::move(name)) {}
    RingBufferFidlErrorHandler() = default;
    void on_fidl_error(fidl::UnbindInfo info) override;
    void handle_unknown_event(fidl::UnknownEventMetadata<ProtocolT> metadata) override {
      ADR_WARN_METHOD() << "RingBufferFidlErrorHandler: unknown event with ordinal "
                        << metadata.event_ordinal;
    }

   protected:
    Device* device() const { return device_; }
    std::string name() const { return name_; }

   private:
    Device* device_;
    ElementId element_id_;
    std::string name_;
  };

  template <typename ProtocolT>
  class FidlErrorHandler : public fidl::AsyncEventHandler<ProtocolT> {
   public:
    FidlErrorHandler(Device* device, std::string name) : device_(device), name_(std::move(name)) {}
    FidlErrorHandler() = default;
    void on_fidl_error(fidl::UnbindInfo info) override;

   protected:
    Device* device() const { return device_; }
    std::string name() const { return name_; }

   private:
    Device* device_;
    std::string name_;
  };

  template <typename ProtocolT>
  class FidlOpenErrorHandler : public FidlErrorHandler<ProtocolT> {
    using FidlErrorHandler<ProtocolT>::FidlErrorHandler;
    void handle_unknown_event(fidl::UnknownEventMetadata<ProtocolT> metadata) override {
      ADR_WARN_METHOD() << "FidlOpenErrorHandler: unknown event with ordinal "
                        << metadata.event_ordinal;
    }
  };

  void Initialize();

  bool AddObserver(const std::shared_ptr<ObserverNotify>& observer_to_add);
  // This also calls AddObserver for the same Notify.
  bool SetControl(std::shared_ptr<ControlNotify> control_notify);

  void DropRingBuffer(ElementId element_id);

  // Translate from the specified client format to the fuchsia_hardware_audio format that the driver
  // can support, including valid_bits_per_sample (which clients don't specify). If the driver
  // cannot satisfy the requested format, `.pcm_format` will be missing in the returned table.
  std::optional<fuchsia_hardware_audio::Format> SupportedDriverFormatForClientFormat(
      ElementId element_id,
      // TODO(https://fxbug.dev/42069015): Consider using media_audio::Format internally.
      const fuchsia_audio::Format& client_format);
  bool SetGain(fuchsia_hardware_audio::GainState& gain_state);

  void SetDaiFormat(ElementId element_id, const fuchsia_hardware_audio::DaiFormat& dai_formats);

  bool Reset();
  bool CodecStart();
  bool CodecStop();

  // These return error codes that can be directly returned to the FIDL client.
  zx_status_t SetTopology(TopologyId topology_id);
  zx_status_t SetElementState(
      ElementId element_id,
      const fuchsia_hardware_audio_signalprocessing::SettableElementState& element_state);

  struct RingBufferInfo {
    fuchsia_audio::RingBuffer ring_buffer;
    fuchsia_audio_device::RingBufferProperties properties;
  };
  bool CreateRingBuffer(
      ElementId element_id, const fuchsia_hardware_audio::Format& format,
      uint32_t requested_ring_buffer_bytes,
      fit::callback<void(
          fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>)>
          create_ring_buffer_callback);

  // Change the channels that are currently active (powered-up).
  bool SetActiveChannels(ElementId element_id, uint64_t channel_bitmask,
                         fit::callback<void(zx::result<zx::time>)> set_active_channels_callback);
  // Start the device ring buffer now (including clock recovery).
  void StartRingBuffer(ElementId element_id,
                       fit::callback<void(zx::result<zx::time>)> start_callback);
  // Stop the device ring buffer now (including device clock recovery).
  void StopRingBuffer(ElementId element_id, fit::callback<void(zx_status_t)> stop_callback);

  // Simple accessors
  // This is the const subset available to device observers.
  //
  bool has_error() const { return (state_ == State::Error); }
  bool is_operational() const { return (state_ == State::Initialized); }
  fuchsia_audio_device::DeviceType device_type() const { return device_type_; }
  bool is_codec() const { return (device_type_ == fuchsia_audio_device::DeviceType::kCodec); }
  bool is_composite() const {
    return (device_type_ == fuchsia_audio_device::DeviceType::kComposite);
  }
  bool is_stream_config_input() const {
    return (device_type_ == fuchsia_audio_device::DeviceType::kInput);
  }
  bool is_stream_config_output() const {
    return (device_type_ == fuchsia_audio_device::DeviceType::kOutput);
  }
  bool is_stream_config() const { return (is_stream_config_input() || is_stream_config_output()); }

  // Assigned by this service, guaranteed unique for this boot session, but not across reboots.
  TokenId token_id() const { return token_id_; }
  // `info` is only populated once the device is initialized.
  const std::optional<fuchsia_audio_device::Info>& info() const { return device_info_; }
  zx::result<zx::clock> GetReadOnlyClock() const;

  const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& dai_format_sets() const {
    return element_dai_format_sets_;
  }
  const std::vector<fuchsia_audio_device::ElementRingBufferFormatSet>& ring_buffer_format_sets()
      const {
    return element_ring_buffer_format_sets_;
  }

  // TODO(https://fxbug.dev/42069015): Consider using media_audio::Format internally.
  fuchsia_audio::Format ring_buffer_format(ElementId element_id) {
    if (const auto match = ring_buffer_map_.find(element_id); match != ring_buffer_map_.end()) {
      return match->second.vmo_format;
    }
    return {};
  }

  std::optional<int16_t> valid_bits_per_sample(ElementId element_id) const {
    auto rb_pair = ring_buffer_map_.find(element_id);
    return (
        (rb_pair == ring_buffer_map_.end() || !rb_pair->second.driver_format ||
         !rb_pair->second.driver_format->pcm_format())
            ? std::nullopt
            : std::optional(rb_pair->second.driver_format->pcm_format()->valid_bits_per_sample()));
  }
  std::optional<bool> supports_set_active_channels(ElementId element_id) const {
    auto rb_pair = ring_buffer_map_.find(element_id);
    return (rb_pair == ring_buffer_map_.end() ? std::nullopt
                                              : rb_pair->second.supports_set_active_channels);
  }

  bool dai_format_is_set() const { return codec_format_.has_value(); }
  const fuchsia_hardware_audio::CodecFormatInfo& codec_format_info(ElementId element_id) const {
    return codec_format_->codec_format_info;
  }
  bool codec_is_started() const { return codec_start_state_.started; }

  bool has_codec_properties() const { return codec_properties_.has_value(); }
  bool has_composite_properties() const { return composite_properties_.has_value(); }
  bool has_stream_config_properties() const { return stream_config_properties_.has_value(); }
  bool has_health_state() const { return health_state_.has_value(); }
  bool dai_format_sets_retrieved() const { return dai_format_sets_retrieved_; }
  bool ring_buffer_format_sets_retrieved() const { return ring_buffer_format_sets_retrieved_; }
  const std::unordered_set<ElementId>& dai_ids() const { return dai_ids_; }
  const std::unordered_set<ElementId>& ring_buffer_ids() const { return ring_buffer_ids_; }
  const std::unordered_set<TopologyId>& topology_ids() const { return topology_ids_; }
  const std::unordered_set<ElementId>& element_ids() const { return element_ids_; }

  bool has_plug_state() const { return plug_state_.has_value(); }
  bool has_gain_state() const { return gain_state_.has_value(); }
  bool checked_for_signalprocessing() const { return supports_signalprocessing_.has_value(); }
  bool supports_signalprocessing() const { return supports_signalprocessing_.value_or(false); }
  void SetSignalProcessingSupported(bool is_supported);

  std::shared_ptr<DeviceInspectInstance> inspect() { return device_inspect_instance_; }

  // Static object counts, for debugging purposes.
  static uint64_t count() { return count_; }
  static uint64_t initialized_count() { return initialized_count_; }
  static uint64_t unhealthy_count() { return unhealthy_count_; }

 private:
  friend class DeviceTestBase;
  friend class DeviceTest;
  friend class CodecTest;
  friend class CompositeTest;
  friend class StreamConfigTest;
  friend class DeviceWarningTest;
  friend class AudioDeviceRegistryServerTestBase;

  static inline const std::string_view kClassName = "Device";
  static inline uint64_t count_ = 0;
  static inline uint64_t initialized_count_ = 0;
  static inline uint64_t unhealthy_count_ = 0;

  Device(std::weak_ptr<DevicePresenceWatcher> presence_watcher, async_dispatcher_t* dispatcher,
         std::string_view name, fuchsia_audio_device::DeviceType device_type,
         fuchsia_audio_device::DriverClient driver_client);

  //
  // Device initialization is essentially a "wait for multiple objects" operation.
  // The following methods query/retrieve the required information during initialization.
  // We use 'Retrieve...' for internal methods, to avoid confusion with the methods on StreamConfig
  // or RingBuffer, which are generally 'Get...'.
  //
  void RetrieveDeviceProperties();
  void RetrieveHealthState();
  void RetrieveSignalProcessingState();
  void RetrieveDaiFormatSets();
  void RetrieveRingBufferFormatSets();
  void RetrievePlugState();
  void RetrieveGainState();

  // Each of the above 'Retrieve...' methods update the related piece of device state, then call
  // either `OnInitializationResponse` or `OnError`.
  void OnInitializationResponse();
  // Upon OnInitializationResponse, this method checks whether all prerequisites are now satisfied.
  bool IsInitializationComplete();
  // Called when a device can be moved into the list of active (successfully initialized) devices.
  void OnInitializationComplete();
  void SetDeviceInfo();

  void OnError(zx_status_t error);
  // Called when a device should be removed altogether -- from the list of active (successfully
  // initialized) devices, or the list of unhealthy devices that previously encountered an error.
  // A non-error removal could occur as a result of USB unplug, for example.
  void OnRemoval();

  template <typename ResultT>
  bool SetDeviceErrorOnFidlError(const ResultT& result, const char* debug_context);
  template <typename ResultT>
  bool SetDeviceErrorOnFidlFrameworkError(const ResultT& result, const char* debug_context);

  fuchsia_audio_device::Info CreateDeviceInfo();
  void CreateDeviceClock();
  void SetHealthState(std::optional<bool> is_healthy);
  void SetPlugState(
      const fuchsia_hardware_audio::PlugState& plug_state,
      std::optional<fuchsia_hardware_audio::PlugDetectCapabilities> plug_detect_capabilities);
  void AddDaiFormatSet(
      ElementId element_id,
      const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets);
  void AddRingBufferFormatSet(
      ElementId id, std::shared_ptr<std::unordered_set<ElementId>>& remaining_ring_buffer_ids,
      const std::vector<fuchsia_hardware_audio::SupportedFormats>& format_set);

  void RetrieveSignalProcessingTopologies();
  void RetrieveSignalProcessingElements();
  void OnSignalProcessingInitializationResponse();

  void RetrieveSignalProcessingTopology();
  void RetrieveSignalProcessingElementStates();
  void RetrieveSignalProcessingElementState(ElementId element_id);

  void GetDaiFormatSets(
      ElementId element_id,
      fit::callback<void(ElementId,
                         const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>&)>
          dai_format_sets_callback);

  // Device-type specific methods used during initialization
  void RetrieveCodecProperties();
  static void SanitizeCodecPropertiesStrings(
      std::optional<fuchsia_hardware_audio::CodecProperties>& codec_properties);

  void RetrieveCompositeProperties();
  static void SanitizeCompositePropertiesStrings(
      std::optional<fuchsia_hardware_audio::CompositeProperties>& composite_properties);

  void RetrieveStreamProperties();
  static void SanitizeStreamPropertiesStrings(
      std::optional<fuchsia_hardware_audio::StreamProperties>& stream_properties);

  std::shared_ptr<ControlNotify> GetControlNotify();
  bool DropControl();
  void ForEachObserver(fit::function<void(std::shared_ptr<ObserverNotify>)> action);

  //
  // # Device state and state machine
  //
  // ## "Forward" transitions
  //
  // - On construction, state is Initializing.  Initialize() kicks off various commands.
  //   Each command then calls either OnInitializationResponse (when completing successfully) or
  //   OnError (if an error occurs at any time).
  //
  // - OnInitializationResponse() changes state to Initialized if all commands are complete;
  //   else state remains Initializing until later OnInitializationResponse().
  //
  // ## "Backward" transitions
  //
  // - OnError() is callable from any internal method, at any time. This transitions the device from
  //   ANY other state to the terminal Error state. Devices in that state ignore all subsequent
  //   OnInitializationResponse / OnError calls or state changes.
  //
  // - Device health is automatically checked at initialization. This may result in OnError
  //   (detailed above). Note that a successful health check is one of the "graduation
  //   requirements" for transitioning to the Initialized state. https://fxbug.dev/42068381
  //   tracks the work to proactively call GetHealthState at some point. We will always surface this
  //   to the client by an error notification, rather than their calling GetHealthState directly.
  //
  enum class State : uint8_t {
    Error,
    Initializing,
    Initialized,
  };
  enum class RingBufferState : uint8_t {
    NotCreated,
    Creating,
    Stopped,
    Started,
  };
  friend std::ostream& operator<<(std::ostream& out, State device_state);
  friend std::ostream& operator<<(std::ostream& out, RingBufferState ring_buffer_state);
  void SetError(zx_status_t error);
  void SetRingBufferState(ElementId element_id, RingBufferState new_ring_buffer_state);

  // Start the ongoing process of device clock recovery from position notifications. Before this
  // call, and after StopDeviceClockRecovery(), the device clock should run at the MONOTONIC rate.
  void RecoverDeviceClockFromPositionInfo();
  void StopDeviceClockRecovery();

  void DeviceDroppedRingBuffer(ElementId element_id);

  // Create the driver RingBuffer FIDL connection.
  fuchsia_audio_device::ControlCreateRingBufferError ConnectRingBufferFidl(
      ElementId element_id, fuchsia_hardware_audio::Format format);
  // Retrieve the underlying RingBufferProperties (turn_on_delay and needs_cache_flush_...).
  void RetrieveRingBufferProperties(ElementId element_id);
  // Post a WatchDelayInfo hanging-get, for external/internal_delay.
  void RetrieveDelayInfo(ElementId element_id);
  // Call the underlying driver RingBuffer::GetVmo method and obtain the VMO and rb_frame_count.
  void GetVmo(ElementId element_id, uint32_t min_frames, uint32_t position_notifications_per_ring);
  // Check whether the 3 prerequisites are in place, for creating a client RingBuffer connection.
  void CheckForRingBufferReady(ElementId element_id);

  // RingBuffer FIDL successful-response handler.
  bool VmoReceived(ElementId element_id) const {
    auto rb_pair = ring_buffer_map_.find(element_id);
    return (rb_pair != ring_buffer_map_.end() &&
            rb_pair->second.num_ring_buffer_frames.has_value());
  }

  void CalculateRequiredRingBufferSizes(ElementId element_id);

  // Device notifies watcher when it completes initialization, encounters an error, or is removed.
  std::weak_ptr<DevicePresenceWatcher> presence_watcher_;
  async_dispatcher_t* dispatcher_;

  // The three values provided upon a successful devfs detection or a Provider/AddDevice call.
  const std::string name_;
  const fuchsia_audio_device::DeviceType device_type_;
  fuchsia_audio_device::DriverClient driver_client_;

  std::optional<fidl::Client<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      sig_proc_client_;
  FidlOpenErrorHandler<fuchsia_hardware_audio_signalprocessing::SignalProcessing> sig_proc_handler_;

  std::optional<fidl::Client<fuchsia_hardware_audio::Codec>> codec_client_;
  FidlErrorHandler<fuchsia_hardware_audio::Codec> codec_handler_;

  std::optional<fidl::Client<fuchsia_hardware_audio::Composite>> composite_client_;
  FidlErrorHandler<fuchsia_hardware_audio::Composite> composite_handler_;

  std::optional<fidl::Client<fuchsia_hardware_audio::StreamConfig>> stream_config_client_;
  FidlErrorHandler<fuchsia_hardware_audio::StreamConfig> stream_config_handler_;

  // Assigned by this service, guaranteed unique for this boot session, but not across reboots.
  const TokenId token_id_;

  State state_;

  // Initialization is complete (state becomes Initialized) when these optionals have values.
  std::optional<fuchsia_hardware_audio::CodecProperties> codec_properties_;
  std::optional<fuchsia_hardware_audio::CompositeProperties> composite_properties_;
  std::optional<fuchsia_hardware_audio::StreamProperties> stream_config_properties_;
  std::optional<std::vector<fuchsia_hardware_audio::SupportedFormats>> ring_buffer_format_sets_;
  std::optional<fuchsia_hardware_audio::GainState> gain_state_;
  std::optional<fuchsia_hardware_audio::PlugState> plug_state_;
  std::optional<bool> health_state_;

  std::optional<bool> supports_signalprocessing_;
  std::vector<fuchsia_hardware_audio_signalprocessing::Element> sig_proc_elements_;
  std::vector<fuchsia_hardware_audio_signalprocessing::Topology> sig_proc_topologies_;
  std::unordered_map<ElementId, ElementRecord> sig_proc_element_map_;

  std::unordered_set<ElementId> dai_ids_;
  std::unordered_set<ElementId> volatile_dai_ids_for_iteration_;
  std::unordered_set<ElementId> ring_buffer_ids_;
  std::unordered_set<ElementId> element_ids_;

  std::unordered_map<TopologyId, std::vector<fuchsia_hardware_audio_signalprocessing::EdgePair>>
      sig_proc_topology_map_;
  std::unordered_set<TopologyId> topology_ids_;
  std::optional<TopologyId> current_topology_id_;

  bool dai_format_sets_retrieved_ = false;
  std::vector<fuchsia_audio_device::ElementDaiFormatSet> element_dai_format_sets_;
  std::unordered_map<ElementId, fuchsia_hardware_audio::DaiFormat> composite_dai_formats_;

  bool ring_buffer_format_sets_retrieved_ = false;
  std::vector<fuchsia_audio_device::ElementRingBufferFormatSet> element_ring_buffer_format_sets_;
  std::vector<std::pair<ElementId, std::vector<fuchsia_hardware_audio::SupportedFormats>>>
      element_driver_ring_buffer_format_sets_;

  struct CodecFormat {
    fuchsia_hardware_audio::DaiFormat dai_format;
    fuchsia_hardware_audio::CodecFormatInfo codec_format_info;
  };
  std::optional<CodecFormat> codec_format_;

  struct CodecStartState {
    bool started = false;
    zx::time start_stop_time = zx::time::infinite_past();
  };
  CodecStartState codec_start_state_;

  std::optional<fuchsia_audio_device::Info> device_info_;

  std::shared_ptr<Clock> device_clock_;

  // Members related to being observed.
  VectorOfWeakPtr<ObserverNotify> observers_;

  // Members related to being controlled.
  std::optional<std::weak_ptr<ControlNotify>> control_notify_;

  // Members related to driver RingBuffer.
  //
  struct RingBufferRecord {
    RingBufferState ring_buffer_state = RingBufferState::NotCreated;

    std::optional<fidl::Client<fuchsia_hardware_audio::RingBuffer>> ring_buffer_client;
    std::unique_ptr<RingBufferFidlErrorHandler<fuchsia_hardware_audio::RingBuffer>>
        ring_buffer_handler;

    fit::callback<void(
        fit::result<fuchsia_audio_device::ControlCreateRingBufferError, Device::RingBufferInfo>)>
        create_ring_buffer_callback;

    // TODO(https://fxbug.dev/42069015): Consider using media_audio::Format internally.
    fuchsia_audio::Format vmo_format;
    zx::vmo ring_buffer_vmo;

    // TODO(https://fxbug.dev/42069014): consider optional<struct>, to minimize separate optionals.
    std::optional<fuchsia_hardware_audio::RingBufferProperties> ring_buffer_properties;
    std::optional<uint32_t> num_ring_buffer_frames;
    std::optional<fuchsia_hardware_audio::DelayInfo> delay_info;
    std::optional<fuchsia_hardware_audio::Format> driver_format;

    uint64_t bytes_per_frame = 0u;
    std::optional<uint32_t> requested_ring_buffer_bytes;
    uint64_t requested_ring_buffer_frames = 0u;

    uint64_t ring_buffer_producer_bytes = 0u;
    uint64_t ring_buffer_consumer_bytes = 0u;

    std::optional<bool> supports_set_active_channels;
    std::optional<uint64_t> active_channels_bitmask;
    std::optional<zx::time> set_active_channels_completed_at;

    std::optional<zx::time> start_time;

    std::shared_ptr<RingBufferInspectInstance> inspect_instance;
  };

  std::unordered_map<ElementId, RingBufferRecord> ring_buffer_map_;

  // Inspect-related
  std::shared_ptr<DeviceInspectInstance> device_inspect_instance_;
};

inline std::ostream& operator<<(std::ostream& out, Device::State device_state) {
  switch (device_state) {
    case Device::State::Error:
      return (out << "Error");
    case Device::State::Initializing:
      return (out << "Initializing");
    case Device::State::Initialized:
      return (out << "Initialized");
    default:
      return (out << "<unknown enum>");
  }
}

inline std::ostream& operator<<(std::ostream& out, Device::RingBufferState ring_buffer_state) {
  switch (ring_buffer_state) {
    case Device::RingBufferState::NotCreated:
      return (out << "NotCreated");
    case Device::RingBufferState::Creating:
      return (out << "Creating");
    case Device::RingBufferState::Stopped:
      return (out << "Stopped");
    case Device::RingBufferState::Started:
      return (out << "Started");
    default:
      return (out << "<unknown enum>");
  }
}

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_

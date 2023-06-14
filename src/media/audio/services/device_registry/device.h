// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fit/function.h>
#include <lib/fit/internal/result.h>
#include <lib/fit/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>

#include <cstdint>
#include <map>
#include <memory>
#include <optional>
#include <string_view>
#include <unordered_set>

#include "src/media/audio/lib/clock/clock.h"
#include "src/media/audio/services/common/vector_of_weak_ptr.h"
#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/observer_notify.h"

namespace media_audio {

// This class represents a driver and audio device, once it is detected.
class Device : public std::enable_shared_from_this<Device>,
               public fidl::AsyncEventHandler<fuchsia_hardware_audio::StreamConfig> {
 public:
  static std::shared_ptr<Device> Create(
      std::weak_ptr<DevicePresenceWatcher> presence_watcher, async_dispatcher_t* dispatcher,
      std::string_view name, fuchsia_audio_device::DeviceType device_type,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config);
  ~Device() override;

  // This is the const subset available to device observers.
  //
  // Assigned by this service, guaranteed unique for this boot session, but not across reboots.
  TokenId token_id() const { return token_id_; }
  // `info` is only populated once the device is initialized.
  const std::optional<fuchsia_audio_device::Info>& info() const { return device_info_; }
  zx::result<zx::clock> GetReadOnlyClock() const;

  bool AddObserver(std::shared_ptr<ObserverNotify> observer_to_add);

  void Initialize();

  void ForEachObserver(fit::function<void(std::shared_ptr<ObserverNotify>)> action);

  bool SetControl(std::shared_ptr<ControlNotify> control_notify);
  bool DropControl();
  void DropRingBuffer();

  // TODO(fxbug.dev/117829): Consider using media_audio::Format internally.
  const fuchsia_audio::Format& ring_buffer_format() { return vmo_format_; }
  std::optional<int16_t> valid_bits_per_sample() const {
    if (!driver_format_ || !driver_format_->pcm_format()) {
      return std::nullopt;
    }
    return driver_format_->pcm_format()->valid_bits_per_sample();
  }

  bool SetGain(fuchsia_hardware_audio::GainState& gain_state);
  void GetCurrentlyPermittedFormats(
      fit::callback<void(std::vector<fuchsia_audio_device::PcmFormatSet>)>
          permitted_formats_callback);
  // Translate from the specified client format to the fuchsia_hardware_audio format that the driver
  // can support, including valid_bits_per_sample (which clients don't specify). If the driver
  // cannot satisfy the requested format, `.pcm_format` will be missing in the returned table.
  std::optional<fuchsia_hardware_audio::Format> SupportedDriverFormatForClientFormat(
      // TODO(fxbug.dev/117829): Consider using media_audio::Format internally.
      const fuchsia_audio::Format& client_format);

  struct RingBufferInfo {
    fuchsia_audio::RingBuffer ring_buffer;
    fuchsia_audio_device::RingBufferProperties properties;
  };
  bool CreateRingBuffer(const fuchsia_hardware_audio::Format& format,
                        uint32_t min_ring_buffer_bytes,
                        fit::callback<void(RingBufferInfo)> create_ring_buffer_callback);

  std::optional<bool> supports_set_active_channels() const { return supports_set_active_channels_; }
  // Change the channels that are currently active (powered-up).
  void SetActiveChannels(uint64_t channel_bitmask,
                         fit::callback<void(zx::result<zx::time>)> set_active_channels_callback);
  // Start the device ring buffer now (including clock recovery).
  void StartRingBuffer(fit::callback<void(zx::result<zx::time>)> start_callback);
  // Stop the device ring buffer now (including device clock recovery).
  void StopRingBuffer(fit::callback<void(zx_status_t)> stop_callback);

  // Static object counts, for debugging purposes.
  static inline uint64_t count() { return count_; }
  static inline uint64_t initialized_count() { return initialized_count_; }
  static inline uint64_t unhealthy_count() { return unhealthy_count_; }

 private:
  friend class DeviceTestBase;
  friend class DeviceTest;
  friend class DeviceWarningTest;
  friend class AudioDeviceRegistryServerTestBase;

  static inline const std::string_view kClassName = "Device";
  static inline uint64_t count_ = 0;
  static inline uint64_t initialized_count_ = 0;
  static inline uint64_t unhealthy_count_ = 0;

  Device(std::weak_ptr<DevicePresenceWatcher> presence_watcher, async_dispatcher_t* dispatcher,
         std::string_view name, fuchsia_audio_device::DeviceType device_type,
         fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config);

  // fidl::AsyncEventHandler<fuchsia_hardware_audio::StreamConfig> implementation,
  // called when the underlying driver disconnects its StreamConfig channel.
  void on_fidl_error(fidl::UnbindInfo info) override;

  // Needed when the underlying driver disconnects its RingBuffer channel.
  class RingBufferErrorHandler
      : public fidl::AsyncEventHandler<fuchsia_hardware_audio::RingBuffer> {
   public:
    explicit RingBufferErrorHandler(Device* device) : device_(device) {}
    RingBufferErrorHandler() = default;
    void on_fidl_error(fidl::UnbindInfo info) override;

   private:
    Device* device_;
  };

  //
  // Actions during the initialization process. We use 'Retrieve...' for internal methods, to avoid
  // confusion with the methods on StreamConfig or RingBuffer, which are generally 'Get...'.
  //
  void RetrieveStreamProperties();
  void RetrieveSupportedFormats();
  void RetrieveGainState();
  void RetrievePlugState();
  void RetrieveHealthState();

  void OnInitializationResponse();
  void OnError(zx_status_t error);
  // Otherwise-normal departure of a device, such as USB device unplug-removal.
  void OnRemoval();

  template <typename ResultT>
  bool LogResultError(const ResultT& result, const char* debug_context);
  template <typename ResultT>
  bool LogResultFrameworkError(const ResultT& result, const char* debug_context);

  static void SanitizeStreamPropertiesStrings(
      std::optional<fuchsia_hardware_audio::StreamProperties>& stream_properties);
  fuchsia_audio_device::Info CreateDeviceInfo();
  void SetDeviceInfo();

  void CreateDeviceClock();

  //
  // # Device state and state machine
  //
  // ## "Forward" transitions
  //
  // - On construction, state is DeviceInitializing.  Initialize() kicks off various commands.
  //   Each command then calls either OnInitializationResponse (when completing successfully) or
  //   OnError (if an error occurs at any time).
  //
  // - OnInitializationResponse() changes state to DeviceInitialized if all commands are complete;
  // else state remains DeviceInitializing until later OnInitializationResponse().
  //
  // ## "Backward" transitions
  //
  // - OnError() is callable from any internal method, at any time. This transitions the device from
  //   ANY other state to the terminal Error state. Devices in that state ignore all subsequent
  //   OnInitializationResponse / OnError calls or state changes.
  //
  // - Device health is automatically checked at initialization. This may result in OnError
  //   (detailed above). Note that a successful health check is one of the "graduation
  //   requirements" for transitioning to the DeviceInitialized state. fxbug.dev/117199 tracks the
  //   work to proactively call GetHealthState at some point. This will always be surfaced to the
  //   client by an error notification, rather than their calling GetHealthState directly.
  //
  enum class State {
    Error,
    DeviceInitializing,
    DeviceInitialized,
    CreatingRingBuffer,
    RingBufferStopped,
    RingBufferStarted,
  };
  friend std::ostream& operator<<(std::ostream& out, State device_state);
  void SetError(zx_status_t error);
  void SetState(State new_state);

  // Start the ongoing process of device clock recovery from position notifications. Before this
  // call, and after StopDeviceClockRecovery(), the device clock should run at the MONOTONIC rate.
  void RecoverDeviceClockFromPositionInfo();
  void StopDeviceClockRecovery();

  void DeviceDroppedRingBuffer();

  // Underlying function called by RetrieveSupportedFormats and GetCurrentlyPermittedFormats.
  void RetrieveFormats(fit::callback<void(std::vector<fuchsia_hardware_audio::SupportedFormats>)>
                           supported_formats_callback);

  // Create the driver RingBuffer FIDL connection.
  bool ConnectRingBufferFidl(fuchsia_hardware_audio::Format format);
  // Retrieve the underlying RingBufferProperties (turn_on_delay and needs_cache_flush_...).
  void RetrieveRingBufferProperties();
  // Post a WatchDelayInfo hanging-get, for external/internal_delay.
  void RetrieveDelayInfo();
  // Call the underlying driver RingBuffer::GetVmo method and obtain the VMO and rb_frame_count.
  void GetVmo(uint32_t min_frames, uint32_t position_notifications_per_ring);
  // Check whether the 3 prerequisites are in place, for creating a client RingBuffer connection.
  void CheckForRingBufferReady();

  // RingBuffer FIDL successful-response handler.
  bool VmoReceived() const { return num_ring_buffer_frames_.has_value(); }

  void CalculateRequiredRingBufferSizes();

  std::shared_ptr<ControlNotify> GetControlNotify();

  // Device notifies watcher when it completes initialization, encounters an error, or is removed.
  std::weak_ptr<DevicePresenceWatcher> presence_watcher_;
  async_dispatcher_t* dispatcher_;

  // The three values provided upon a successful devfs detection or a Provider/AddDevice call.
  const std::string name_;
  const fuchsia_audio_device::DeviceType device_type_;
  fidl::Client<fuchsia_hardware_audio::StreamConfig> stream_config_;

  // Assigned by this service, guaranteed unique for this boot session, but not across reboots.
  const TokenId token_id_;

  // Initialization is complete when these 5 optionals are populated.
  std::optional<fuchsia_hardware_audio::StreamProperties> stream_config_properties_;
  std::optional<std::vector<fuchsia_hardware_audio::SupportedFormats>> supported_formats_;
  std::optional<fuchsia_hardware_audio::GainState> gain_state_;
  std::optional<fuchsia_hardware_audio::PlugState> plug_state_;
  std::optional<bool> health_state_;

  State state_{State::DeviceInitializing};

  std::optional<fuchsia_audio_device::Info> device_info_;

  std::shared_ptr<Clock> device_clock_;
  std::vector<fuchsia_audio_device::PcmFormatSet> permitted_formats_;

  // Members related to being observed.
  VectorOfWeakPtr<ObserverNotify> observers_;

  // Members related to being controlled.
  std::optional<std::weak_ptr<ControlNotify>> control_notify_;

  // Members related to driver RingBuffer.
  fidl::Client<fuchsia_hardware_audio::RingBuffer> ring_buffer_client_;
  RingBufferErrorHandler ring_buffer_handler_;

  fit::callback<void(RingBufferInfo)> create_ring_buffer_callback_;
  // TODO(fxbug.dev/117829): Consider using media_audio::Format internally.
  fuchsia_audio::Format vmo_format_;
  zx::vmo ring_buffer_vmo_;

  // TODO(fxbug.dev/117828): consider using an optional<struct> to minimize separate optionals.
  std::optional<fuchsia_hardware_audio::RingBufferProperties> ring_buffer_properties_;
  std::optional<uint32_t> num_ring_buffer_frames_;
  std::optional<fuchsia_hardware_audio::DelayInfo> delay_info_;
  std::optional<fuchsia_hardware_audio::Format> driver_format_;

  uint64_t bytes_per_frame_ = 0;
  std::optional<uint32_t> requested_ring_buffer_bytes_;
  uint64_t requested_ring_buffer_frames_;

  uint64_t ring_buffer_producer_bytes_;
  uint64_t ring_buffer_consumer_bytes_;

  std::optional<bool> supports_set_active_channels_;
  uint64_t active_channels_bitmask_;
  std::optional<zx::time> active_channels_set_time_;

  std::optional<zx::time> start_time_;
};

inline std::ostream& operator<<(std::ostream& out, Device::State device_state) {
  switch (device_state) {
    case Device::State::Error:
      return (out << "Error");
    case Device::State::DeviceInitializing:
      return (out << "DeviceInitializing");
    case Device::State::DeviceInitialized:
      return (out << "DeviceInitialized");
    case Device::State::CreatingRingBuffer:
      return (out << "CreatingRingBuffer");
    case Device::State::RingBufferStopped:
      return (out << "RingBufferStopped");
    case Device::State::RingBufferStarted:
      return (out << "RingBufferStarted");
  }
}

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_

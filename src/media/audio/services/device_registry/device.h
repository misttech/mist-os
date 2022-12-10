// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mediastreams/cpp/natural_types.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fit/internal/result.h>
#include <lib/fit/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <zircon/system/public/zircon/errors.h>

#include <map>
#include <memory>
#include <optional>
#include <string_view>

#include "src/media/audio/lib/clock/clock.h"
#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

class Device : public std::enable_shared_from_this<Device>,
               public fidl::AsyncEventHandler<fuchsia_hardware_audio::StreamConfig> {
 public:
  static std::shared_ptr<Device> Create(
      std::weak_ptr<DevicePresenceWatcher> presence_watcher, async_dispatcher_t* dispatcher,
      std::string_view name, fuchsia_audio_device::DeviceType device_type,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config);
  ~Device() override;

  void Initialize();

  // Assigned by this service, guaranteed unique for this boot session, but not across reboots.
  TokenId token_id() const { return token_id_; }
  // This can only be called once the device is Ready.
  const std::optional<fuchsia_audio_device::Info>& info() { return device_info_; }

  void QueryHealthState();
  zx::result<zx::clock> GetReadOnlyClock();

  // Static object counts, for debugging purposes.
  static uint32_t count() { return count_; }
  static uint32_t initialized_count() { return initialized_count_; }
  static uint32_t unhealthy_count() { return unhealthy_count_; }

  //
  // # Device state and state machine
  //
  // ## "Forward" transitions
  //
  // - On construction, state is Initializing.  Initialize() kicks off various commands.
  //   Each command then calls either OnInitializationResponse (when completing successfully) or
  //   OnError (if an error occurs at any time).
  //
  // - OnInitializationResponse() checks whether all commands are complete, and if so changes state
  //   to Ready. If not, the state remains Initializing until subsequent OnInitializationResponse().
  //
  // ## "Backward" transitions
  //
  // - OnError() is callable from any internal method, at any time. This transitions the device from
  //   ANY other state to the terminal Error state. Devices in that state ignore all subsequent
  //   OnInitializationResponse / OnHealthResponse / OnError calls or state changes.
  //
  // - Device health can be queried proactively by the client and is automatically checked at
  //   certain times (including during initialization). These result in either OnError
  //   (detailed above) or OnHealthResponse. Note that a successful health check is one of the
  //   "graduation requirements" for transitioning to the Ready state: thus Initializing devices
  //   treat OnHealthResponse identically to OnInitializationResponse. OnHealthResponse has no
  //   effect on the state of devices that are not Initializing (Error remains Error, Ready remains
  //   Ready, etc).
  enum class State {
    Initializing,
    Error,
    Ready,
  };

 private:
  friend class DeviceTestBase;
  friend class DeviceTest;
  friend class DeviceWarningTest;

  static inline const std::string_view kClassName = "Device";
  static uint32_t count_;
  static uint32_t initialized_count_;
  static uint32_t unhealthy_count_;

  Device(std::weak_ptr<DevicePresenceWatcher> presence_watcher, async_dispatcher_t* dispatcher,
         std::string_view name, fuchsia_audio_device::DeviceType device_type,
         fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config);

  // fidl::AsyncEventHandler<fuchsia_hardware_audio::StreamConfig> implementation,
  // called when the underlying driver disconnects its StreamConfig channel.
  void on_fidl_error(fidl::UnbindInfo error) override;

  //
  // Actions during the initialization process.
  //
  void QueryStreamProperties();
  void QuerySupportedFormats();
  void QueryGainState();
  void QueryPlugState();

  void OnInitializationResponse();
  void OnHealthResponse();
  void OnError(zx_status_t error);
  // Otherwise-normal departure of a device, such as USB device unplug-removal.
  void OnRemoval();

  template <typename ResultT>
  bool LogResultError(const ResultT& result, const char* debug_context);

  fuchsia_audio_device::Info CreateDeviceInfo();
  void SetDeviceInfo();

  void CreateDeviceClock();

  void SetStateError(zx_status_t error);
  void SetStateReady();

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
  std::optional<std::vector<fuchsia_hardware_audio::SupportedFormats>> formats_;
  std::optional<fuchsia_hardware_audio::GainState> gain_state_;
  std::optional<fuchsia_hardware_audio::PlugState> plug_state_;
  std::optional<fuchsia_hardware_audio::HealthState> health_state_;

  State state_{State::Initializing};

  std::optional<fuchsia_audio_device::Info> device_info_;

  std::shared_ptr<Clock> device_clock_;
};

inline std::ostream& operator<<(std::ostream& out, Device::State device_state) {
  switch (device_state) {
    case Device::State::Initializing:
      return (out << "Initializing");
    case Device::State::Error:
      return (out << "Error");
    case Device::State::Ready:
      return (out << "Ready");
  }
}

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_H_

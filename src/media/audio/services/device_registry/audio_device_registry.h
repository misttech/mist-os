// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_AUDIO_DEVICE_REGISTRY_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_AUDIO_DEVICE_REGISTRY_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.hardware.audio/cpp/markers.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>

#include <cstdint>
#include <memory>
#include <unordered_set>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/device_presence_watcher.h"

namespace media_audio {

class ProviderServer;

class Device;
class DeviceDetector;

class AudioDeviceRegistry : public std::enable_shared_from_this<AudioDeviceRegistry>,
                            public DevicePresenceWatcher {
 public:
  explicit AudioDeviceRegistry(std::shared_ptr<FidlThread> server_thread);
  ~AudioDeviceRegistry() override;

  // Kick off device-detection. Devices auto-initialize and call one of the methods below.
  zx_status_t StartDeviceDetection();
  zx_status_t RegisterAndServeOutgoing();

  // Device support
  // The set of active (successfully initialized) devices.
  const std::unordered_set<std::shared_ptr<Device>>& devices() { return devices_; }
  // The set of devices that encountered an error. A device should never be in both sets.
  const std::unordered_set<std::shared_ptr<Device>>& unhealthy_devices() {
    return unhealthy_devices_;
  }
  // Add a newly-constructed Device object to our "initializing" list.
  void AddDevice(std::shared_ptr<Device> initializing_device);

  // DevicePresenceWatcher interface
  // Add a successfully-initialized Device to our active list.
  void DeviceIsReady(std::shared_ptr<Device> ready_device) final;
  void DeviceHasError(std::shared_ptr<Device> device_with_error) final;
  void DeviceIsRemoved(std::shared_ptr<Device> device_to_remove) final;

  // Provider support
  std::shared_ptr<ProviderServer> CreateProviderServer(
      fidl::ServerEnd<fuchsia_audio_device::Provider> server_end);

 private:
  static inline const std::string_view kClassName = "AudioDeviceRegistry";

  std::shared_ptr<DeviceDetector> device_detector_;

  // These devices are in the process of being initialized.
  std::unordered_set<std::shared_ptr<Device>> pending_devices_;

  // The list of operational devices, provided to clients (via Registry/WatchDevicesAdded).
  std::unordered_set<std::shared_ptr<Device>> devices_;

  // These devices have encountered an error or have self-reported as unhealthy. They have been
  // reported (via Registry/WatchDeviceRemoved) as being removed, and are no longer in the device
  // list provided to clients (via Registry/WatchDevicesAdded).
  std::unordered_set<std::shared_ptr<Device>> unhealthy_devices_;

  std::shared_ptr<FidlThread> thread_;
  component::OutgoingDirectory outgoing_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_AUDIO_DEVICE_REGISTRY_H_

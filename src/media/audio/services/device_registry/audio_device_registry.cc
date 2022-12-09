// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/audio_device_registry.h"

#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio.device/cpp/wire.h>
#include <lib/fidl/cpp/wire/internal/transport.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <memory>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_detector.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

AudioDeviceRegistry::AudioDeviceRegistry(std::shared_ptr<FidlThread> server_thread)
    : thread_(std::move(server_thread)) {
  ADR_LOG_OBJECT(kLogObjectLifetimes);
}

AudioDeviceRegistry::~AudioDeviceRegistry() { ADR_LOG_OBJECT(kLogObjectLifetimes); }

zx_status_t AudioDeviceRegistry::StartDeviceDetection() {
  DeviceDetectionHandler device_detection_handler =
      [this](std::string_view name, fuchsia_audio_device::DeviceType device_type,
             fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> stream_config_client) {
        ADR_LOG_OBJECT(kLogDeviceDetection)
            << "detected Audio " << device_type << " '" << name << "'";
        FX_CHECK(stream_config_client);

        AddDevice(Device::Create(this->shared_from_this(), thread_->dispatcher(), name, device_type,
                                 std::move(stream_config_client)));
      };

  auto detector_result =
      media_audio::DeviceDetector::Create(device_detection_handler, thread_->dispatcher());

  ADR_LOG_OBJECT(kLogDeviceDetection) << "returning " << detector_result.status_value();

  if (detector_result.is_ok()) {
    device_detector_ = detector_result.value();
  }

  return detector_result.status_value();
}

void AudioDeviceRegistry::AddDevice(std::shared_ptr<Device> initializing_device) {
  pending_devices_.insert(initializing_device);
}

void AudioDeviceRegistry::DeviceIsReady(std::shared_ptr<media_audio::Device> ready_device) {
  ADR_LOG_OBJECT(kLogAudioDeviceRegistryMethods) << "for device " << ready_device;

  if (!pending_devices_.erase(ready_device)) {
    FX_LOGS(ERROR) << __func__ << ": device " << ready_device << " not found in pending list";
  }

  if (!devices_.insert(ready_device).second) {
    FX_LOGS(ERROR) << __func__ << ": device " << ready_device << " already in initialized list";
    return;
  }
  // for (auto& registry : registries_) {   registry->DeviceWasAdded(ready_device);  }
}

void AudioDeviceRegistry::DeviceHasError(std::shared_ptr<media_audio::Device> device_with_error) {
  if (devices_.erase(device_with_error)) {
    FX_LOGS(WARNING) << __func__ << " for previously-initialized device " << device_with_error;
  }

  if (pending_devices_.erase(device_with_error)) {
    FX_LOGS(WARNING) << __func__ << " for pending (initializing) device " << device_with_error;
  }

  if (!unhealthy_devices_.insert(device_with_error).second) {
    FX_LOGS(WARNING) << __func__ << ": device " << device_with_error
                     << " had previously encountered an error - no change";
    return;
  }

  FX_LOGS(WARNING) << __func__ << ": added device " << device_with_error << " to unhealthy list";
}

// Entirely remove the device, including from the unhealthy list.
void AudioDeviceRegistry::DeviceIsRemoved(std::shared_ptr<media_audio::Device> device_to_remove) {
  ADR_LOG_OBJECT(kLogAudioDeviceRegistryMethods) << "for device " << device_to_remove;

  if (devices_.erase(device_to_remove)) {
    ADR_LOG_OBJECT(kLogObjectLifetimes)
        << "removed " << device_to_remove << " from active device list";
  }

  if (unhealthy_devices_.erase(device_to_remove)) {
    ADR_LOG_OBJECT(kLogObjectLifetimes)
        << "removed " << device_to_remove << " from unhealthy device list";
  }

  if (pending_devices_.erase(device_to_remove)) {
    ADR_LOG_OBJECT(kLogObjectLifetimes)
        << "removed " << device_to_remove << " from pending (initializing) device list";
  }
}

}  // namespace media_audio

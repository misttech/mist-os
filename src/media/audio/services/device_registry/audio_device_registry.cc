// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/audio_device_registry.h"

#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio.device/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/internal/transport.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <memory>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_detector.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/provider_server.h"
#include "src/media/audio/services/device_registry/registry_server.h"

namespace media_audio {

AudioDeviceRegistry::AudioDeviceRegistry(std::shared_ptr<FidlThread> server_thread)
    : thread_(std::move(server_thread)),
      outgoing_(component::OutgoingDirectory(thread_->dispatcher())) {
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

  // Notify registry clients of this new device.
  for (auto& weak_registry : registries_) {
    if (std::shared_ptr<RegistryServer> registry = weak_registry.lock()) {
      registry->DeviceWasAdded(ready_device);
    }
  }
}

void AudioDeviceRegistry::DeviceHasError(std::shared_ptr<media_audio::Device> device_with_error) {
  if (devices_.erase(device_with_error)) {
    FX_LOGS(WARNING) << __func__ << " for previously-initialized device " << device_with_error;

    // Device should have already notified any other associated objects.
    NotifyRegistriesOfDeviceRemoval(device_with_error->token_id());
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
    // Device should have already notified any other associated objects.
    NotifyRegistriesOfDeviceRemoval(device_to_remove->token_id());

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

// Notify registry clients of this device departure (whether from surprise-removal or error).
void AudioDeviceRegistry::NotifyRegistriesOfDeviceRemoval(uint64_t removed_device_id) {
  ADR_LOG_OBJECT(kLogAudioDeviceRegistryMethods);

  for (auto weak_it = registries_.begin(); weak_it != registries_.end(); ++weak_it) {
    if (auto registry = weak_it->lock()) {
      registry->DeviceWasRemoved(removed_device_id);
    }
  }
}

zx_status_t AudioDeviceRegistry::RegisterAndServeOutgoing() {
  ADR_LOG_OBJECT(kLogAudioDeviceRegistryMethods);

  auto status = outgoing_.AddProtocol<fuchsia_audio_device::Provider>(
      [this](fidl::ServerEnd<fuchsia_audio_device::Provider> server_end) mutable {
        ADR_LOG_OBJECT(kLogProviderServerMethods)
            << "Incoming connection for fuchsia.audio.device.Provider";

        auto provider = CreateProviderServer(std::move(server_end));
      });

  if (status.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Provider protocol: " << status.error_value() << " ("
                   << status.status_string() << ")";
    return status.status_value();
  }

  status = outgoing_.AddProtocol<fuchsia_audio_device::Registry>(
      [this](fidl::ServerEnd<fuchsia_audio_device::Registry> server_end) mutable {
        ADR_LOG_OBJECT(kLogRegistryServerMethods)
            << "Incoming connection for fuchsia.audio.device.Registry";

        auto registry = CreateRegistryServer(std::move(server_end));
      });
  if (status.is_error()) {
    FX_LOGS(ERROR) << "Failed to add Registry protocol: " << status.error_value() << " ("
                   << status.status_string() << ")";
    return status.status_value();
  }

  // Set up an outgoing directory with the startup handle (provided by the system to components so
  // they can serve out FIDL protocols etc).
  return outgoing_.ServeFromStartupInfo().status_value();
}

// This does nothing that couldn't be done by calling ProviderServer::Create directly, bit it
// mirrors similar (upcoming) methods for other FIDL Server classes that WILL do more. If this turns
// out to not be the case, we will remove this at that time.
std::shared_ptr<ProviderServer> AudioDeviceRegistry::CreateProviderServer(
    fidl::ServerEnd<fuchsia_audio_device::Provider> server_end) {
  ADR_LOG_OBJECT(kLogAudioDeviceRegistryMethods || kLogProviderServerMethods);

  return ProviderServer::Create(thread_, std::move(server_end), shared_from_this());
}

std::shared_ptr<RegistryServer> AudioDeviceRegistry::CreateRegistryServer(
    fidl::ServerEnd<fuchsia_audio_device::Registry> server_end) {
  ADR_LOG_OBJECT(kLogAudioDeviceRegistryMethods || kLogRegistryServerMethods);

  auto new_registry = RegistryServer::Create(thread_, std::move(server_end), shared_from_this());
  registries_.push_back(new_registry);

  for (const auto& device : devices()) {
    new_registry->DeviceWasAdded(device);
  }
  return new_registry;
}

}  // namespace media_audio

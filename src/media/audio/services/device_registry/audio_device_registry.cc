// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/audio_device_registry.h"

#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/internal/transport.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>

#include <memory>
#include <utility>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_detector.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/observer_server.h"
#include "src/media/audio/services/device_registry/provider_server.h"
#include "src/media/audio/services/device_registry/registry_server.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {

using DriverClient = fuchsia_audio_device::DriverClient;

AudioDeviceRegistry::AudioDeviceRegistry(std::shared_ptr<FidlThread> server_thread)
    : thread_(std::move(server_thread)),
      outgoing_(component::OutgoingDirectory(thread_->dispatcher())) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
}

AudioDeviceRegistry::~AudioDeviceRegistry() { ADR_LOG_METHOD(kLogObjectLifetimes); }

zx_status_t AudioDeviceRegistry::StartDeviceDetection() {
  DeviceDetectionHandler device_detection_handler =
      [this](std::string_view name, fuchsia_audio_device::DeviceType device_type,
             fuchsia_audio_device::DriverClient driver_client) {
        ADR_LOG_OBJECT(kLogDeviceDetection)
            << "detected Audio " << device_type << " '" << name << "'";

        switch (driver_client.Which()) {
          case fuchsia_audio_device::DriverClient::Tag::kCodec:
            FX_CHECK(driver_client.codec()->is_valid());
            break;
          case fuchsia_audio_device::DriverClient::Tag::kStreamConfig:
            FX_CHECK(driver_client.stream_config()->is_valid());
            break;
          case fuchsia_audio_device::DriverClient::Tag::kComposite:
            ADR_WARN_OBJECT() << "Composite device detected but not yet supported";
            return;
          case fuchsia_audio_device::DriverClient::Tag::kDai:
            ADR_WARN_OBJECT() << "Dai device detected but not yet supported";
            return;
          default:
            FX_CHECK(!driver_client.IsUnknown());
        }
        AddDevice(Device::Create(this->shared_from_this(), thread_->dispatcher(), name, device_type,
                                 std::move(driver_client)));
      };

  auto detector_result =
      media_audio::DeviceDetector::Create(device_detection_handler, thread_->dispatcher());

  ADR_LOG_METHOD(kLogDeviceDetection) << "returning " << detector_result.status_value();

  if (detector_result.is_ok()) {
    device_detector_ = detector_result.value();
  }

  return detector_result.status_value();
}

void AudioDeviceRegistry::AddDevice(const std::shared_ptr<Device>& initializing_device) {
  pending_devices_.insert(initializing_device);
}

void AudioDeviceRegistry::DeviceIsReady(std::shared_ptr<media_audio::Device> ready_device) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods) << "for device " << ready_device;

  if (!pending_devices_.erase(ready_device)) {
    FX_LOGS(ERROR) << __func__ << ": device " << ready_device << " not found in pending list";
  }

  // We don't check the removed device list, because TokenIds are uint64_t created incrementally;
  // it is galactically improbable that TokenIds will ever rollover. We don't check the unhealthy
  // device list, because (for now) an unhealthy device cannot "get well"; it must be entirely
  // powered-down/removed before it can rejoin the party (thus see "removed device list" above).

  if (!devices_.insert(ready_device).second) {
    FX_LOGS(ERROR) << __func__ << ": device " << ready_device << " already in initialized list";
    return;
  }

  // Notify registry clients of this new device.
  for (auto& weak_registry : registries_) {
    if (auto registry = weak_registry.lock(); registry) {
      registry->DeviceWasAdded(ready_device);
    }
  }
}

void AudioDeviceRegistry::DeviceHasError(std::shared_ptr<media_audio::Device> device_with_error) {
  if (devices_.erase(device_with_error)) {
    ADR_WARN_METHOD() << "for previously-initialized device " << device_with_error;

    // Device should have already notified any other associated objects.
    NotifyRegistriesOfDeviceRemoval(device_with_error->token_id());
  }

  if (pending_devices_.erase(device_with_error)) {
    ADR_WARN_METHOD() << "for pending (initializing) device " << device_with_error;
  }

  if (!unhealthy_devices_.insert(device_with_error).second) {
    ADR_WARN_METHOD() << "device " << device_with_error
                      << " had previously encountered an error - no change";
    return;
  }

  ADR_WARN_METHOD() << "added device " << device_with_error << " to unhealthy list";
}

// Entirely remove the device, including from the unhealthy list.
void AudioDeviceRegistry::DeviceIsRemoved(std::shared_ptr<media_audio::Device> device_to_remove) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods) << "for device " << device_to_remove;

  if (devices_.erase(device_to_remove)) {
    // Device should have already notified any other associated objects.
    NotifyRegistriesOfDeviceRemoval(device_to_remove->token_id());

    ADR_LOG_METHOD(kLogObjectLifetimes)
        << "removed " << device_to_remove << " from active device list";
  }

  if (pending_devices_.erase(device_to_remove)) {
    ADR_LOG_METHOD(kLogObjectLifetimes)
        << "removed " << device_to_remove << " from pending (initializing) device list";
  }

  if (unhealthy_devices_.erase(device_to_remove)) {
    ADR_LOG_METHOD(kLogObjectLifetimes)
        << "removed " << device_to_remove << " from unhealthy device list";
  }
}

std::pair<AudioDeviceRegistry::DevicePresence, std::shared_ptr<Device>>
AudioDeviceRegistry::FindDeviceByTokenId(TokenId token_id) {
  for (auto& device : devices_) {
    if (device->token_id() == token_id) {
      ADR_LOG_STATIC(kLogAudioDeviceRegistryMethods) << "active (token_id " << token_id << ")";
      return std::make_pair(DevicePresence::Active, device);
    }
  }
  for (auto& device : unhealthy_devices_) {
    if (device->token_id() == token_id) {
      ADR_WARN_METHOD() << "device (token_id " << token_id << ") has an error";
      return std::make_pair(DevicePresence::Error, nullptr);
    }
  }
  // For initializing devices and completely unknown devices, we return the same error.
  for (auto& device : pending_devices_) {
    if (device->token_id() == token_id) {
      ADR_WARN_METHOD() << "device not yet ready (token_id " << token_id << ")";
      break;
    }
  }
  ADR_WARN_METHOD() << "no known device (token_id " << token_id << ")";
  return std::make_pair(DevicePresence::Unknown, nullptr);
}

bool AudioDeviceRegistry::ClaimDeviceForControl(std::shared_ptr<Device> device,
                                                std::shared_ptr<ControlNotify> notify) {
  return device->SetControl(std::move(notify));
}

// Notify registry clients of this device departure (whether from surprise-removal or error).
void AudioDeviceRegistry::NotifyRegistriesOfDeviceRemoval(uint64_t removed_device_id) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods);

  for (auto& weak_registry : registries_) {
    if (auto registry = weak_registry.lock(); registry) {
      registry->DeviceWasRemoved(removed_device_id);
    }
  }
}

zx_status_t AudioDeviceRegistry::RegisterAndServeOutgoing() {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods);

  auto status = outgoing_.AddUnmanagedProtocol<fuchsia_audio_device::Provider>(
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

  status = outgoing_.AddUnmanagedProtocol<fuchsia_audio_device::Registry>(
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

  status = outgoing_.AddUnmanagedProtocol<fuchsia_audio_device::ControlCreator>(
      [this](fidl::ServerEnd<fuchsia_audio_device::ControlCreator> server_end) mutable {
        ADR_LOG_OBJECT(kLogControlCreatorServerMethods)
            << "Incoming connection for fuchsia.audio.device.ControlCreator";

        auto control_creator = CreateControlCreatorServer(std::move(server_end));
      });
  if (status.is_error()) {
    FX_LOGS(ERROR) << "Failed to add ControlCreator protocol: " << status.error_value() << " ("
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
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods || kLogProviderServerMethods);

  return ProviderServer::Create(thread_, std::move(server_end), shared_from_this());
}

std::shared_ptr<RegistryServer> AudioDeviceRegistry::CreateRegistryServer(
    fidl::ServerEnd<fuchsia_audio_device::Registry> server_end) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods || kLogRegistryServerMethods);

  auto new_registry = RegistryServer::Create(thread_, std::move(server_end), shared_from_this());
  registries_.push_back(new_registry);

  for (const auto& device : devices()) {
    new_registry->DeviceWasAdded(device);
  }
  return new_registry;
}

std::shared_ptr<ControlCreatorServer> AudioDeviceRegistry::CreateControlCreatorServer(
    fidl::ServerEnd<fuchsia_audio_device::ControlCreator> server_end) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods || kLogControlCreatorServerMethods);

  return ControlCreatorServer::Create(thread_, std::move(server_end), shared_from_this());
}

std::shared_ptr<ObserverServer> AudioDeviceRegistry::CreateObserverServer(
    fidl::ServerEnd<fuchsia_audio_device::Observer> server_end,
    std::shared_ptr<Device> observed_device) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods || kLogObserverServerMethods);

  auto observer = ObserverServer::Create(thread_, std::move(server_end), observed_device);
  observed_device->AddObserver(observer);
  return observer;
}

std::shared_ptr<ControlServer> AudioDeviceRegistry::CreateControlServer(
    fidl::ServerEnd<fuchsia_audio_device::Control> server_end,
    std::shared_ptr<Device> device_to_control) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods || kLogControlServerMethods);

  auto control =
      ControlServer::Create(thread_, std::move(server_end), shared_from_this(), device_to_control);

  if (device_to_control->SetControl(control)) {
    return control;
  }
  control->Shutdown();
  return nullptr;
}

std::shared_ptr<RingBufferServer> AudioDeviceRegistry::CreateRingBufferServer(
    fidl::ServerEnd<fuchsia_audio_device::RingBuffer> server_end,
    const std::shared_ptr<ControlServer>& parent,
    const std::shared_ptr<Device>& device_to_control) {
  ADR_LOG_METHOD(kLogAudioDeviceRegistryMethods || kLogRingBufferServerMethods);

  return RingBufferServer::Create(thread_, std::move(server_end), parent, device_to_control);
}

}  // namespace media_audio

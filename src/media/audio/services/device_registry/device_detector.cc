// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device_detector.h"

#include <fcntl.h>
#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <memory>
#include <vector>

#include <fbl/unique_fd.h>

#include "src/lib/fsl/io/device_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

using fuchsia_audio_device::DeviceType;
using fuchsia_audio_device::DriverClient;

namespace {

struct DeviceNodeSpecifier {
  const char* path;
  DeviceType device_type;
};

constexpr DeviceNodeSpecifier kAudioDevNodes[] = {
    {.path = "/dev/class/audio-composite", .device_type = DeviceType::kComposite},
    {.path = "/dev/class/audio-input", .device_type = DeviceType::kInput},
    {.path = "/dev/class/audio-output", .device_type = DeviceType::kOutput},
    {.path = "/dev/class/codec", .device_type = DeviceType::kCodec},
};

}  // namespace

zx::result<std::shared_ptr<DeviceDetector>> DeviceDetector::Create(DeviceDetectionHandler handler,
                                                                   async_dispatcher_t* dispatcher) {
  // The constructor is private, forcing clients to use DeviceDetector::Create().
  class MakePublicCtor : public DeviceDetector {
   public:
    MakePublicCtor(DeviceDetectionHandler handler, async_dispatcher_t* dispatcher)
        : DeviceDetector(std::move(handler), dispatcher) {}
  };

  auto detector = std::make_shared<MakePublicCtor>(std::move(handler), dispatcher);

  if (auto status = detector->StartDeviceWatchers(); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(detector);
}

zx_status_t DeviceDetector::StartDeviceWatchers() {
  // StartDeviceWatchers should never be called a second time.
  FX_CHECK(watchers_.empty());
  FX_CHECK(dispatcher_);

  for (const auto& dev_node : kAudioDevNodes) {
    auto watcher = fsl::DeviceWatcher::Create(
        dev_node.path,
        [this, device_type = dev_node.device_type](
            const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
          if (!dispatcher_) {
            FX_LOGS(ERROR) << "DeviceWatcher fired but dispatcher is gone";
            return;
          }
          if (device_type == DeviceType::kCodec || device_type == DeviceType::kComposite ||
              device_type == DeviceType::kInput || device_type == DeviceType::kOutput) {
            DriverClientFromDevFs(dir, filename, device_type);
          } else {
            FX_LOGS(WARNING) << device_type << " device detection not yet supported";
            return;
          }
        },
        dispatcher_);

    // If any of our directory-monitors cannot be created, destroy them all and fail.
    if (watcher == nullptr) {
      FX_LOGS(ERROR) << "DeviceDetector failed to create DeviceWatcher for '" << dev_node.path
                     << "'; stopping all device monitoring.";
      watchers_.clear();
      handler_ = nullptr;
      return ZX_ERR_INTERNAL;
    }
    watchers_.emplace_back(std::move(watcher));
  }

  return ZX_OK;
}

void DeviceDetector::DriverClientFromDevFs(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                           const std::string& name, DeviceType device_type) {
  FX_CHECK(handler_);

  std::optional<fuchsia_audio_device::DriverClient> driver_client;

  if (device_type == fuchsia_audio_device::DeviceType::kCodec) {
    zx::result client_end = component::ConnectAt<fuchsia_hardware_audio::CodecConnector>(dir, name);
    if (client_end.is_error()) {
      FX_PLOGS(ERROR, client_end.error_value())
          << "DeviceDetector failed to connect to device node at '" << name << "'";
      return;
    }
    fidl::Client connector(std::move(client_end.value()), dispatcher_);
    auto [client, server] = fidl::Endpoints<fuchsia_hardware_audio::Codec>::Create();
    auto status = connector->Connect(std::move(server));
    if (!status.is_ok()) {
      FX_PLOGS(ERROR, status.error_value().status())
          << "Connector/Connect failed for " << device_type;
      return;
    }
    driver_client = DriverClient::WithCodec(std::move(client));
  } else if (device_type == fuchsia_audio_device::DeviceType::kComposite) {
    // Composite devices such as aml-g12-tdm are DFv2: we can connect directly to them.
    // TODO(https://fxbug.dev/304551042): Convert VirtualAudioComposite to DFv2; remove 'else'.
    if constexpr (kDetectDFv2CompositeDevices) {
      zx::result client_end = component::ConnectAt<fuchsia_hardware_audio::Composite>(dir, name);
      if (client_end.is_error()) {
        FX_PLOGS(ERROR, client_end.error_value())
            << "DeviceDetector failed to connect to DFv2 Composite node at '" << name << "'";
        return;
      }
      driver_client = DriverClient::WithComposite(std::move(client_end.value()));
    }
    // The VirtualAudioComposite implementation is DFv1: connect via CompositeConnector.
    else {
      zx::result client_end =
          component::ConnectAt<fuchsia_hardware_audio::CompositeConnector>(dir, name);
      if (client_end.is_error()) {
        FX_PLOGS(ERROR, client_end.error_value())
            << "DeviceDetector failed to connect to device node at '" << name << "'";
        return;
      }
      fidl::Client connector(std::move(client_end.value()), dispatcher_);
      auto [client, server] = fidl::Endpoints<fuchsia_hardware_audio::Composite>::Create();
      auto status = connector->Connect(std::move(server));
      if (!status.is_ok()) {
        FX_PLOGS(ERROR, status.error_value().status())
            << "Connector/Connect failed for " << device_type;
        return;
      }
      driver_client = DriverClient::WithComposite(std::move(client));
    }
  } else if (device_type == fuchsia_audio_device::DeviceType::kInput ||
             device_type == fuchsia_audio_device::DeviceType::kOutput) {
    zx::result client_end =
        component::ConnectAt<fuchsia_hardware_audio::StreamConfigConnector>(dir, name);
    if (client_end.is_error()) {
      FX_PLOGS(ERROR, client_end.error_value())
          << "DeviceDetector failed to connect to device node at '" << name << "'";
      return;
    }
    fidl::Client connector(std::move(client_end.value()), dispatcher_);
    auto [client, server] = fidl::Endpoints<fuchsia_hardware_audio::StreamConfig>::Create();
    auto status = connector->Connect(std::move(server));
    if (!status.is_ok()) {
      FX_PLOGS(ERROR, status.error_value().status())
          << "Connector/Connect failed for " << device_type;
      return;
    }
    driver_client = DriverClient::WithStreamConfig(std::move(client));
  } else {
    FX_LOGS(WARNING) << device_type << " device detection not yet supported";
    return;
  }

  if constexpr (kLogDeviceDetection) {
    FX_LOGS(INFO) << "Detected and connected to " << device_type << " '" << name << "'";
  }

  handler_(name, device_type, std::move(*driver_client));
}

}  // namespace media_audio

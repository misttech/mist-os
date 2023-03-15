// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device_detector.h"

#include <fcntl.h>
#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <memory>
#include <vector>

#include <fbl/unique_fd.h>

#include "src/lib/fsl/io/device_watcher.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

namespace {

struct DeviceNodeSpecifier {
  const char* path;
  fuchsia_audio_device::DeviceType device_type;
};

constexpr DeviceNodeSpecifier kAudioDevNodes[] = {
    {.path = "/dev/class/audio-output", .device_type = fuchsia_audio_device::DeviceType::kOutput},
    {.path = "/dev/class/audio-input", .device_type = fuchsia_audio_device::DeviceType::kInput},
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
        [this, device_type = dev_node.device_type](int dir_fd, std::string_view filename) {
          if (dispatcher_) {
            StreamConfigFromDevFs(dir_fd, filename, device_type);
          } else {
            FX_LOGS(ERROR) << "DeviceWatcher fired but dispatcher is gone";
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

void DeviceDetector::StreamConfigFromDevFs(int dir_fd, std::string_view name,
                                           fuchsia_audio_device::DeviceType device_type) {
  FX_CHECK(handler_);

  fdio_cpp::UnownedFdioCaller caller(dir_fd);
  zx::result client_end =
      component::ConnectAt<fuchsia_hardware_audio::StreamConfigConnector>(caller.directory(), name);
  if (client_end.is_error()) {
    FX_PLOGS(ERROR, client_end.error_value())
        << "DeviceDetector failed to connect to device node at '" << name << "'";
    return;
  }
  fidl::Client config_connector(std::move(client_end.value()), dispatcher_);

  auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::StreamConfig>();
  if (!endpoints.is_ok()) {
    FX_LOGS(ERROR) << "CreateEndpoints<fuchsia_hardware_audio::StreamConfig> failed";
    return;
  }

  auto status = config_connector->Connect(std::move(endpoints->server));
  if (!status.is_ok()) {
    FX_PLOGS(ERROR, status.error_value().status()) << "StreamConfigConnector/Connect failed";
    return;
  }

  if constexpr (kLogDeviceDetection) {
    FX_LOGS(INFO) << "Detected and connected to " << device_type << " '" << name << "'";
  }
  handler_(name, device_type, std::move(endpoints->client));
}

}  // namespace media_audio

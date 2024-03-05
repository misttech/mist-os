// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/provider_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/fit/internal/result.h>
#include <lib/syslog/cpp/macros.h>

#include <optional>

#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

using DriverClient = fuchsia_audio_device::DriverClient;

// static
std::shared_ptr<ProviderServer> ProviderServer::Create(
    std::shared_ptr<const FidlThread> thread,
    fidl::ServerEnd<fuchsia_audio_device::Provider> server_end,
    std::shared_ptr<AudioDeviceRegistry> parent) {
  ADR_LOG_STATIC(kLogProviderServerMethods);

  return BaseFidlServer::Create(std::move(thread), std::move(server_end), parent);
}

ProviderServer::ProviderServer(std::shared_ptr<AudioDeviceRegistry> parent) : parent_(parent) {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  ++count_;
  LogObjectCounts();
}

ProviderServer::~ProviderServer() {
  ADR_LOG_METHOD(kLogObjectLifetimes);
  --count_;
  LogObjectCounts();
}

void ProviderServer::AddDevice(AddDeviceRequest& request, AddDeviceCompleter::Sync& completer) {
  ADR_LOG_METHOD(kLogProviderServerMethods);

  if (!request.device_name() || request.device_name()->empty()) {
    ADR_WARN_METHOD() << "device_name was absent/empty";
    completer.Reply(fit::error(fuchsia_audio_device::ProviderAddDeviceError::kInvalidName));
    return;
  }

  if (!request.device_type()) {
    ADR_WARN_METHOD() << "device_type was absent";
    completer.Reply(fit::error(fuchsia_audio_device::ProviderAddDeviceError::kInvalidType));
    return;
  }

  if (!request.stream_config()) {
    ADR_WARN_METHOD() << "stream_config was absent";

    completer.Reply(fit::error(fuchsia_audio_device::ProviderAddDeviceError::kInvalidStreamConfig));
    return;
  }

  ADR_LOG_METHOD(kLogDeviceDetection)
      << "request to add " << *request.device_type() << " '" << *request.device_name() << "'";

  // This kicks off device initialization, which notifies the parent when it completes.
  parent_->AddDevice(
      Device::Create(parent_, thread().dispatcher(), *request.device_name(), *request.device_type(),
                     DriverClient::WithStreamConfig(std::move(*request.stream_config()))));

  fuchsia_audio_device::ProviderAddDeviceResponse response{};
  completer.Reply(fit::success(response));
}

}  // namespace media_audio

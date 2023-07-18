// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_STUB_REGISTRY_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_STUB_REGISTRY_SERVER_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <optional>
#include <string_view>
#include <utility>

#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/common/fidl_thread.h"

namespace media_audio {

// FIDL server for fuchsia_audio_device/Registry (a stub "do-nothing" implementation).
class StubRegistryServer
    : public BaseFidlServer<StubRegistryServer, fidl::Server, fuchsia_audio_device::Registry> {
 public:
  static std::shared_ptr<StubRegistryServer> Create(
      std::shared_ptr<const FidlThread> thread,
      fidl::ServerEnd<fuchsia_audio_device::Registry> server_end) {
    FX_LOGS(INFO) << kClassName << "::" << __FUNCTION__;
    return BaseFidlServer::Create(std::move(thread), std::move(server_end));
  }

  // fuchsia.audio.device.Registry implementation
  void WatchDevicesAdded(WatchDevicesAddedCompleter::Sync& completer) final {
    if (watch_devices_added_completer_) {
      FX_LOGS(WARNING) << kClassName << "::" << __FUNCTION__
                       << ": previous request has not yet completed";
      completer.Reply(fit::error<fuchsia_audio_device::RegistryWatchDevicesAddedError>(
          fuchsia_audio_device::RegistryWatchDevicesAddedError::kWatchAlreadyPending));
      return;
    }
    FX_LOGS(INFO) << kClassName << "::" << __FUNCTION__;
    watch_devices_added_completer_ = completer.ToAsync();
  }

  void WatchDeviceRemoved(WatchDeviceRemovedCompleter::Sync& completer) final {
    if (watch_device_removed_completer_) {
      FX_LOGS(WARNING) << kClassName << "::" << __FUNCTION__
                       << ": previous request has not yet completed";
      completer.Reply(fit::error<fuchsia_audio_device::RegistryWatchDeviceRemovedError>(
          fuchsia_audio_device::RegistryWatchDeviceRemovedError::kWatchAlreadyPending));
      return;
    }
    FX_LOGS(INFO) << kClassName << "::" << __FUNCTION__;
    watch_device_removed_completer_ = completer.ToAsync();
  }

  void CreateObserver(CreateObserverRequest& request,
                      CreateObserverCompleter::Sync& completer) final {
    FX_LOGS(INFO) << kClassName << "::" << __FUNCTION__;
    completer.Reply(fit::success(fuchsia_audio_device::RegistryCreateObserverResponse{}));
  }

 private:
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  static inline const std::string_view kClassName = "StubRegistryServer";

  StubRegistryServer() = default;

  std::optional<WatchDevicesAddedCompleter::Async> watch_devices_added_completer_;
  std::optional<WatchDeviceRemovedCompleter::Async> watch_device_removed_completer_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_STUB_REGISTRY_SERVER_H_

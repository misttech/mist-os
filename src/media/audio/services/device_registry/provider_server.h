// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_PROVIDER_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_PROVIDER_SERVER_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>

#include <cstdint>
#include <memory>

#include "src/media/audio/services/common/base_fidl_server.h"

namespace media_audio {

class AudioDeviceRegistry;

// FIDL server for fuchsia_audio_device/Provider. This interface programmatically adds audio devices
// (instead of their being automatically detected in `devfs`).
class ProviderServer
    : public std::enable_shared_from_this<ProviderServer>,
      public BaseFidlServer<ProviderServer, fidl::Server, fuchsia_audio_device::Provider> {
 public:
  static std::shared_ptr<ProviderServer> Create(
      std::shared_ptr<const FidlThread> thread,
      fidl::ServerEnd<fuchsia_audio_device::Provider> server_end,
      std::shared_ptr<AudioDeviceRegistry> parent);
  ~ProviderServer() override;

  // fuchsia.audio.device.Provider implementation
  void AddDevice(AddDeviceRequest& request, AddDeviceCompleter::Sync& completer) override;

  // Static object count, for debugging purposes.
  static inline uint64_t count() { return count_; }

 private:
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  static inline const std::string_view kClassName = "ProviderServer";
  static inline uint64_t count_ = 0;

  explicit ProviderServer(std::shared_ptr<AudioDeviceRegistry> parent);

  std::shared_ptr<AudioDeviceRegistry> parent_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_PROVIDER_SERVER_H_

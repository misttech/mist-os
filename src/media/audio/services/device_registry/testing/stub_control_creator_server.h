// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_STUB_CONTROL_CREATOR_SERVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_STUB_CONTROL_CREATOR_SERVER_H_

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fit/internal/result.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <string_view>

#include "src/media/audio/services/common/base_fidl_server.h"
#include "src/media/audio/services/common/fidl_thread.h"

namespace media_audio {

// FIDL server for fuchsia_audio_device/ControlCreator (a stub "do-nothing" implementation).
class StubControlCreatorServer : public BaseFidlServer<StubControlCreatorServer, fidl::Server,
                                                       fuchsia_audio_device::ControlCreator> {
 public:
  static std::shared_ptr<StubControlCreatorServer> Create(
      std::shared_ptr<const FidlThread> thread,
      fidl::ServerEnd<fuchsia_audio_device::ControlCreator> server_end) {
    FX_LOGS(INFO) << kClassName << "::" << __FUNCTION__;
    return BaseFidlServer::Create(std::move(thread), std::move(server_end));
  }

  // fuchsia.audio.device.ControlCreator implementation
  void Create(CreateRequest& request, CreateCompleter::Sync& completer) override {
    FX_LOGS(INFO) << kClassName << "::" << __FUNCTION__;
    completer.Reply(fit::success(fuchsia_audio_device::ControlCreatorCreateResponse{}));
  }

 private:
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  static inline const std::string_view kClassName = "StubControlCreatorServer";

  StubControlCreatorServer() = default;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_STUB_CONTROL_CREATOR_SERVER_H_

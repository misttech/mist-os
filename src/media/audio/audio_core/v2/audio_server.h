// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_SERVER_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_SERVER_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>

#include <map>
#include <memory>
#include <optional>
#include <utility>

#include "src/media/audio/audio_core/v2/renderer_capturer_creator.h"
#include "src/media/audio/services/common/base_fidl_server.h"

namespace media_audio {

class AudioServer : public BaseFidlServer<AudioServer, fidl::WireServer, fuchsia_media::Audio>,
                    public std::enable_shared_from_this<AudioServer> {
 public:
  static std::shared_ptr<AudioServer> Create(std::shared_ptr<const FidlThread> fidl_thread,
                                             fidl::ServerEnd<fuchsia_media::Audio> server_end,
                                             std::shared_ptr<RendererCapturerCreator> creator);

  //
  // Implementation of fidl::WireServer<fuchsia_media::Audio>.
  //

  void CreateAudioRenderer(CreateAudioRendererRequestView request,
                           CreateAudioRendererCompleter::Sync& completer) final;

  void CreateAudioCapturer(CreateAudioCapturerRequestView request,
                           CreateAudioCapturerCompleter::Sync& completer) final;

 private:
  static inline constexpr std::string_view kClassName = "AudioServer";
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  explicit AudioServer(std::shared_ptr<RendererCapturerCreator> creator)
      : creator_(std::move(creator)) {}

  const std::shared_ptr<RendererCapturerCreator> creator_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_AUDIO_SERVER_H_

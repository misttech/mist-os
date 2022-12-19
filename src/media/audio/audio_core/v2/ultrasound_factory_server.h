// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_ULTRASOUND_FACTORY_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_ULTRASOUND_FACTORY_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.ultrasound/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>

#include <map>
#include <memory>
#include <optional>
#include <utility>

#include "src/media/audio/audio_core/v2/renderer_capturer_creator.h"
#include "src/media/audio/lib/format2/format.h"
#include "src/media/audio/services/common/base_fidl_server.h"

namespace media_audio {

class UltrasoundFactoryServer
    : public BaseFidlServer<UltrasoundFactoryServer, fidl::WireServer, fuchsia_ultrasound::Factory>,
      public std::enable_shared_from_this<UltrasoundFactoryServer> {
 public:
  struct Args {
    std::shared_ptr<RendererCapturerCreator> creator;
    Format renderer_format;
    Format capturer_format;
  };
  static std::shared_ptr<UltrasoundFactoryServer> Create(
      std::shared_ptr<const FidlThread> fidl_thread,
      fidl::ServerEnd<fuchsia_ultrasound::Factory> server_end, Args args);

  //
  // Implementation of fidl::WireServer<fuchsia_ultrasound::Factory>.
  //

  void CreateRenderer(CreateRendererRequestView request,
                      CreateRendererCompleter::Sync& completer) final;

  void CreateCapturer(CreateCapturerRequestView request,
                      CreateCapturerCompleter::Sync& completer) final;

 private:
  static inline constexpr std::string_view kClassName = "UltrasoundFactoryServer";
  template <typename ServerT, template <typename T> typename FidlServerT, typename ProtocolT>
  friend class BaseFidlServer;

  explicit UltrasoundFactoryServer(Args args)
      : creator_(std::move(args.creator)),
        renderer_format_(std::move(args.renderer_format)),
        capturer_format_(std::move(args.capturer_format)) {}

  const std::shared_ptr<RendererCapturerCreator> creator_;
  const Format renderer_format_;
  const Format capturer_format_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_ULTRASOUND_FACTORY_H_

// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_RENDERER_CAPTURER_CREATOR_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_RENDERER_CAPTURER_CREATOR_H_

#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <fidl/fuchsia.media/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fit/function.h>
#include <lib/zx/clock.h>

#include <map>
#include <memory>
#include <optional>
#include <utility>

#include "src/media/audio/audio_core/shared/device_config.h"
#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/v2/audio_capturer_server.h"
#include "src/media/audio/audio_core/v2/audio_renderer_server.h"
#include "src/media/audio/audio_core/v2/route_graph.h"
#include "src/media/audio/lib/format2/format.h"

namespace media_audio {

class RendererCapturerCreator : public std::enable_shared_from_this<RendererCapturerCreator> {
 public:
  RendererCapturerCreator(
      std::shared_ptr<const FidlThread> fidl_thread,
      std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client,
      std::shared_ptr<RouteGraph> route_graph);

  // Both of these methods follow the same pattern: they call CreateGraphControlledClock, then
  // construct an Audio{Renderer,Capturer}Server using the given parameters, where the server's
  // `default_reference_clock` is the newly-created graph-controlled clock. If the caller needs
  // immediate access to this clock, they can supply a `notify_clock` callback.

  void CreateRenderer(fidl::ServerEnd<fuchsia_media::AudioRenderer> server_end,
                      media::audio::RenderUsage usage, std::optional<Format> format,
                      fit::callback<void(const zx::clock&)> notify_clock);

  void CreateCapturer(fidl::ServerEnd<fuchsia_media::AudioCapturer> server_end,
                      media::audio::CaptureUsage usage, std::optional<Format> format,
                      fit::callback<void(const zx::clock&)> notify_clock);

 private:
  void WithNewGraphControlledClock(fit::callback<void(zx::clock, zx::eventpair)> fn);

  const std::shared_ptr<const FidlThread> fidl_thread_;
  const std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  const std::shared_ptr<RouteGraph> route_graph_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_RENDERER_CAPTURER_CREATOR_H_

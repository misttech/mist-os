// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/renderer_capturer_creator.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

namespace media_audio {

namespace {

using ::media::audio::CaptureUsage;
using ::media::audio::RenderUsage;

}  // namespace

RendererCapturerCreator::RendererCapturerCreator(
    std::shared_ptr<const FidlThread> fidl_thread,
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client,
    std::shared_ptr<RouteGraph> route_graph)
    : fidl_thread_(std::move(fidl_thread)),
      graph_client_(std::move(graph_client)),
      route_graph_(std::move(route_graph)) {}

void RendererCapturerCreator::CreateRenderer(
    fidl::ServerEnd<fuchsia_media::AudioRenderer> server_end, RenderUsage usage,
    std::optional<Format> format) {
  WithNewGraphControlledClock([this, self = shared_from_this(), server_end = std::move(server_end),
                               usage, format](auto clock, auto release_fence) mutable {
    AudioRendererServer::Create(
        fidl_thread_, std::move(server_end),
        {
            .graph_client = graph_client_,
            .usage = usage,
            .format = std::move(format),
            .default_reference_clock = std::move(clock),
            .ramp_on_play_pause = usage != RenderUsage::ULTRASOUND,

            // Once the renderer is fully created, it can be routed.
            .on_fully_created =
                [route_graph = route_graph_](auto server) { route_graph->AddRenderer(server); },

            // Once the renderer has shutdown, it can be unrouted (if it was previously routed).
            // We hold the release fence until shutdown so the mixer continues to adjust `clock`
            // until this renderer shuts down.
            .on_shutdown =
                [route_graph = route_graph_, fence = std::move(release_fence)](auto server) {
                  if (server->IsFullyCreated()) {
                    route_graph->RemoveRenderer(server);
                  }
                },
        });
  });
}

void RendererCapturerCreator::CreateCapturer(
    fidl::ServerEnd<fuchsia_media::AudioCapturer> server_end, CaptureUsage usage,
    std::optional<Format> format) {
  WithNewGraphControlledClock([this, self = shared_from_this(), server_end = std::move(server_end),
                               usage, format](auto clock, auto release_fence) mutable {
    AudioCapturerServer::Create(
        fidl_thread_, std::move(server_end),
        {
            .graph_client = graph_client_,
            .usage = usage,
            .format = std::move(format),
            .default_reference_clock = std::move(clock),

            // Once the capturer is fully created, it can be routed.
            .on_fully_created =
                [route_graph = route_graph_](auto server) { route_graph->AddCapturer(server); },

            // Once the capturer has shutdown, it can be unrouted (if it was previously routed).
            // We hold the release fence until shutdown so the mixer continues to adjust `clock`
            // until this renderer shuts down.
            .on_shutdown =
                [route_graph = route_graph_, fence = std::move(release_fence)](auto server) {
                  if (server->IsFullyCreated()) {
                    route_graph->RemoveCapturer(server);
                  }
                },
        });
  });
}

void RendererCapturerCreator::WithNewGraphControlledClock(
    fit::callback<void(zx::clock, zx::eventpair)> fn) {
  fidl::Arena<> arena;
  (*graph_client_)
      ->CreateGraphControlledReferenceClock()
      .Then([fn = std::move(fn)](auto& result) mutable {
        if (!result.ok() || !result->is_ok()) {
          FX_LOGS(WARNING) << "CreateGraphControlledReferenceClock: failed with transport_error="
                           << result
                           << ", response_status=" << (result.ok() ? result->error_value() : 0);
          return;
        }
        if (!result->value()->has_reference_clock() || !result->value()->has_release_fence()) {
          FX_LOGS(ERROR) << "CreateGraphControlledReferenceClock bug: response missing fields";
          return;
        }
        fn(std::move(result->value()->reference_clock()),
           std::move(result->value()->release_fence()));
      });
}

}  // namespace media_audio

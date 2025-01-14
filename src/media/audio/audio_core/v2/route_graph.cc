// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/route_graph.h"

#include <lib/syslog/cpp/macros.h>

#include <algorithm>

namespace media_audio {

namespace {

using ::media::audio::CaptureUsage;
using ::media::audio::RenderUsage;
using ::media::audio::VolumeCurve;

template <class InfoT, class PipelineT>
typename std::vector<InfoT>::iterator FindDevice(std::vector<InfoT>& devices,
                                                 const std::shared_ptr<PipelineT>& pipeline) {
  return std::find_if(devices.begin(), devices.end(),
                      [&pipeline](const auto& info) { return pipeline == info.pipeline; });
}

template <class InfoT>
void SortByPlugTime(std::vector<InfoT>& devices) {
  // Sort with most-recently-plugged devices first. Use a stable sort so that ties are
  // broken by most-recently-added device, which helps make unit tests deterministic.
  std::stable_sort(devices.begin(), devices.end(),
                   [](const auto& a, const auto& b) { return a.plug_time > b.plug_time; });
}

template <class InfoT, class PipelineT>
void AddDevice(std::vector<InfoT>& devices, const std::shared_ptr<PipelineT>& pipeline,
               zx::time plug_time) {
  FX_CHECK(FindDevice(devices, pipeline) == devices.end());
  devices.push_back({pipeline, plug_time});
  SortByPlugTime(devices);
}

template <class InfoT, class PipelineT>
void RemoveDevice(std::vector<InfoT>& devices, std::shared_ptr<PipelineT> pipeline) {
  auto it = FindDevice(devices, pipeline);
  FX_CHECK(it != devices.end());
  devices.erase(it);
  SortByPlugTime(devices);
}

fidl::VectorView<GainControlId> GainControlsVector(fidl::AnyArena& arena,
                                                   const std::vector<GainControlId>& usage_controls,
                                                   GainControlId stream_control,
                                                   std::optional<GainControlId> ramp_control) {
  size_t count = usage_controls.size() + 1;
  if (ramp_control) {
    count++;
  }

  fidl::VectorView<GainControlId> out(arena, count);
  std::copy(usage_controls.begin(), usage_controls.end(), out.begin());
  auto idx = usage_controls.size();

  out[idx] = stream_control;
  if (ramp_control) {
    out[idx + 1] = *ramp_control;
  }

  return out;
}

}  // namespace

RouteGraph::RouteGraph(
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client)
    : graph_client_(std::move(graph_client)) {}

void RouteGraph::AddOutputDevice(std::shared_ptr<OutputDevicePipeline> pipeline,
                                 zx::time plug_time) {
  AddDevice(output_devices_, pipeline, plug_time);
  RecomputeGainControlsForRenderers();
  RerouteAllRenderers();

  if (pipeline->loopback()) {
    AddInputDevice(pipeline->loopback(), plug_time);
  }
}

void RouteGraph::AddInputDevice(std::shared_ptr<InputDevicePipeline> pipeline, zx::time plug_time) {
  AddDevice(input_devices_, pipeline, plug_time);
  RecomputeGainControlsForCapturers();
  RerouteAllCapturers();
}

void RouteGraph::RemoveOutputDevice(std::shared_ptr<OutputDevicePipeline> pipeline) {
  RemoveDevice(output_devices_, pipeline);
  RecomputeGainControlsForRenderers();
  RerouteAllRenderers();

  if (pipeline->loopback()) {
    RemoveInputDevice(pipeline->loopback());
  }
}

void RouteGraph::RemoveInputDevice(std::shared_ptr<InputDevicePipeline> pipeline) {
  RemoveDevice(input_devices_, pipeline);
  RecomputeGainControlsForCapturers();
  RerouteAllCapturers();
}

void RouteGraph::AddRenderer(std::shared_ptr<AudioRendererServer> renderer) {
  FX_CHECK(renderers_.emplace(renderer, RendererInfo{}).second);
  RerouteRenderer(renderer);
}

void RouteGraph::RemoveRenderer(std::shared_ptr<AudioRendererServer> renderer) {
  auto it = renderers_.find(renderer);
  FX_CHECK(it != renderers_.end());
  DisconnectRenderer(it);
}

void RouteGraph::AddCapturer(std::shared_ptr<AudioCapturerServer> capturer) {
  FX_CHECK(capturers_.emplace(capturer, CapturerInfo{}).second);
  RerouteCapturer(capturer);
}

void RouteGraph::RemoveCapturer(std::shared_ptr<AudioCapturerServer> capturer) {
  auto it = capturers_.find(capturer);
  FX_CHECK(it != capturers_.end());
  DisconnectCapturer(it);
}

std::shared_ptr<VolumeCurve> RouteGraph::VolumeCurveForUsage(RenderUsage usage) {
  for (const auto& device : output_devices_) {
    if (device.pipeline->SupportsUsage(usage)) {
      return device.pipeline->volume_curve();
    }
  }
  return nullptr;
}

std::shared_ptr<VolumeCurve> RouteGraph::VolumeCurveForUsage(CaptureUsage usage) {
  for (const auto& device : input_devices_) {
    if (device.pipeline->SupportsUsage(usage)) {
      return device.pipeline->volume_curve();
    }
  }
  return nullptr;
}

void RouteGraph::RecomputeGainControlsForRenderers() {
  gain_controls_per_render_usage_.clear();

  for (const auto& device : output_devices_) {
    for (auto usage : media::audio::kRenderUsages) {
      if (auto uv = device.pipeline->UsageVolumeForUsage(usage); uv) {
        gain_controls_per_render_usage_[usage] = uv->gain_controls();
      }
    }
  }
}

void RouteGraph::RecomputeGainControlsForCapturers() {
  gain_controls_per_capture_usage_.clear();

  for (const auto& device : input_devices_) {
    for (auto usage : media::audio::kCaptureUsages) {
      if (auto uv = device.pipeline->UsageVolumeForUsage(usage); uv) {
        gain_controls_per_capture_usage_[usage] = uv->gain_controls();
      }
    }
  }
}

void RouteGraph::RerouteAllRenderers() {
  for (const auto& [renderer, info] : renderers_) {
    RerouteRenderer(renderer);
  }
}

void RouteGraph::RerouteAllCapturers() {
  for (const auto& [capturer, info] : capturers_) {
    RerouteCapturer(capturer);
  }
}

void RouteGraph::RerouteRenderer(const std::shared_ptr<AudioRendererServer>& renderer) {
  auto it = renderers_.find(renderer);
  FX_CHECK(it != renderers_.end());
  auto& info = it->second;

  std::shared_ptr<OutputDevicePipeline> pipeline;
  for (const auto& p : output_devices_) {
    if (p.pipeline->SupportsUsage(renderer->usage())) {
      pipeline = p.pipeline;
      break;
    }
  }

  // If there is nowhere to route this renderer, ensure it is disconnected.
  if (!pipeline) {
    DisconnectRenderer(it);
    return;
  }

  auto dest_node = pipeline->DestNodeForUsage(renderer->usage());

  // Already routed to the correct place.
  if (info.dest_node == dest_node) {
    return;
  }
  // If currently routed to the wrong place, disconnect before rerouting.
  if (info.dest_node) {
    DisconnectRenderer(it);
  }

  fidl::Arena<> arena;
  (*graph_client_)
      ->CreateEdge(
          fuchsia_audio_mixer::wire::GraphCreateEdgeRequest::Builder(arena)
              .source_id(renderer->producer_node())
              .dest_id(dest_node)
              .gain_controls(GainControlsVector(
                  arena, gain_controls_per_render_usage_[renderer->usage()],
                  renderer->stream_gain_control(), renderer->play_pause_ramp_gain_control()))
              .Build())
      .Then([](auto& result) {
        if (result.ok() && !result->is_ok()) {
          FX_LOGS(ERROR) << "CreateEdge failed with code: "
                         << fidl::ToUnderlying(result->error_value());
          return;
        }
      });

  info.dest_node = dest_node;
}

void RouteGraph::RerouteCapturer(const std::shared_ptr<AudioCapturerServer>& capturer) {
  auto it = capturers_.find(capturer);
  FX_CHECK(it != capturers_.end());
  auto& info = it->second;

  std::shared_ptr<InputDevicePipeline> pipeline;
  for (const auto& p : input_devices_) {
    if (p.pipeline->SupportsUsage(capturer->usage())) {
      pipeline = p.pipeline;
      break;
    }
  }

  // If there is nowhere to route this capturer, ensure it is disconnected.
  if (!pipeline) {
    DisconnectCapturer(it);
    return;
  }

  auto source_node = pipeline->SourceNodeForFormat(capturer->format());

  // If the source doesn't exist yet, create it, then try again.
  if (!source_node) {
    pipeline->CreateSourceNodeForFormat(capturer->format(),
                                        [this, self = shared_from_this(), capturer](auto node) {
                                          if (!node) {
                                            return;
                                          }
                                          // Try again only if the capturer was not removed before
                                          // this completed. The target device may have been removed
                                          // as well, but that's ok; if so, the next RerouteCapturer
                                          // call will recompute the target device.
                                          if (capturers_.find(capturer) == capturers_.end()) {
                                            return;
                                          }
                                          RerouteCapturer(capturer);
                                        });
    return;
  }
  // Already routed to the correct place.
  if (info.source_node == source_node) {
    return;
  }
  // If currently routed to the wrong place, disconnect before rerouting.
  if (info.source_node) {
    DisconnectCapturer(it);
  }

  fidl::Arena<> arena;

  (*graph_client_)
      ->CreateEdge(fuchsia_audio_mixer::wire::GraphCreateEdgeRequest::Builder(arena)
                       .source_id(*source_node)
                       .dest_id(capturer->consumer_node())
                       .gain_controls(GainControlsVector(
                           arena, gain_controls_per_capture_usage_[capturer->usage()],
                           capturer->stream_gain_control(), std::nullopt))
                       .Build())
      .Then([](auto& result) {
        if (result.ok() && !result->is_ok()) {
          FX_LOGS(ERROR) << "CreateEdge failed with code: "
                         << fidl::ToUnderlying(result->error_value());
          return;
        }
      });

  info.source_node = source_node;
}

void RouteGraph::DisconnectRenderer(RendererMap::iterator it) {
  auto& renderer = it->first;
  auto& info = it->second;

  if (!info.dest_node) {
    return;
  }

  fidl::Arena<> arena;
  (*graph_client_)
      ->DeleteEdge(fuchsia_audio_mixer::wire::GraphDeleteEdgeRequest::Builder(arena)
                       .source_id(renderer->producer_node())
                       .dest_id(*info.dest_node)
                       .Build())
      .Then([](auto& result) {
        if (result.ok() && !result->is_ok()) {
          FX_LOGS(ERROR) << "DeleteEdge failed with code: "
                         << fidl::ToUnderlying(result->error_value());
        }
      });

  info.dest_node = std::nullopt;
}

void RouteGraph::DisconnectCapturer(CapturerMap::iterator it) {
  auto& capturer = it->first;
  auto& info = it->second;

  if (!info.source_node) {
    return;
  }

  fidl::Arena<> arena;
  (*graph_client_)
      ->DeleteEdge(fuchsia_audio_mixer::wire::GraphDeleteEdgeRequest::Builder(arena)
                       .source_id(*info.source_node)
                       .dest_id(capturer->consumer_node())
                       .Build())
      .Then([](auto& result) {
        if (result.ok() && !result->is_ok()) {
          FX_LOGS(ERROR) << "DeleteEdge failed with code: "
                         << fidl::ToUnderlying(result->error_value());
        }
      });

  info.source_node = std::nullopt;
}

}  // namespace media_audio

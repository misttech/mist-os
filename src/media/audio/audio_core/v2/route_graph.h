// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_ROUTE_GRAPH_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_ROUTE_GRAPH_H_

#include <lib/fit/function.h>

#include <map>
#include <memory>
#include <utility>

#include "src/media/audio/audio_core/shared/stream_usage.h"
#include "src/media/audio/audio_core/v2/audio_capturer_server.h"
#include "src/media/audio/audio_core/v2/audio_renderer_server.h"
#include "src/media/audio/audio_core/v2/graph_types.h"
#include "src/media/audio/audio_core/v2/input_device_pipeline.h"
#include "src/media/audio/audio_core/v2/output_device_pipeline.h"

namespace media_audio {

// Manages renderer-to-output-device and input-device-to-capturer connections.
class RouteGraph : public std::enable_shared_from_this<RouteGraph> {
 public:
  explicit RouteGraph(
      std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client);

  // Adds a routable device. This should be called when the device has been added and plugged in.
  // Caller is expected to `Start` the device. If the output device has a loopback interface, then
  // `pipeline->loopback()` is automatically added as well.
  void AddOutputDevice(std::shared_ptr<OutputDevicePipeline> pipeline, zx::time plug_time);
  void AddInputDevice(std::shared_ptr<InputDevicePipeline> pipeline, zx::time plug_time);

  // Removes a routable pipeline. This should be called when the device has been removed or
  // unplugged. Caller is expected to `Stop` the pipeline.
  void RemoveOutputDevice(std::shared_ptr<OutputDevicePipeline> pipeline);
  void RemoveInputDevice(std::shared_ptr<InputDevicePipeline> pipeline);

  // Adds a routable renderer or capturer.
  void AddRenderer(std::shared_ptr<AudioRendererServer> renderer);
  void AddCapturer(std::shared_ptr<AudioCapturerServer> capturer);

  // Removes a routable renderer or capturer.
  void RemoveRenderer(std::shared_ptr<AudioRendererServer> renderer);
  void RemoveCapturer(std::shared_ptr<AudioCapturerServer> capturer);

  // Returns the VolumeCurve currently used by the given usage, or nullptr if the current usage
  // cannot be routed to any devices.
  std::shared_ptr<media::audio::VolumeCurve> VolumeCurveForUsage(media::audio::RenderUsage usage);
  std::shared_ptr<media::audio::VolumeCurve> VolumeCurveForUsage(media::audio::CaptureUsage usage);

 private:
  struct OutputDevicePipelineInfo {
    std::shared_ptr<OutputDevicePipeline> pipeline;
    zx::time plug_time;
  };
  struct InputDevicePipelineInfo {
    std::shared_ptr<InputDevicePipeline> pipeline;
    zx::time plug_time;
  };
  struct RendererInfo {
    std::optional<NodeId> dest_node;  // if currently connected
  };
  struct CapturerInfo {
    std::optional<NodeId> source_node;  // if currently connected
  };

  // Using std::map so iteration order is deterministic, which helps make tests deterministic.
  using RendererMap = std::map<std::shared_ptr<AudioRendererServer>, RendererInfo>;
  using CapturerMap = std::map<std::shared_ptr<AudioCapturerServer>, CapturerInfo>;

  void RecomputeGainControlsForRenderers();
  void RecomputeGainControlsForCapturers();
  void RerouteAllRenderers();
  void RerouteAllCapturers();
  void RerouteRenderer(const std::shared_ptr<AudioRendererServer>& renderer);
  void RerouteCapturer(const std::shared_ptr<AudioCapturerServer>& capturer);
  void DisconnectRenderer(RendererMap::iterator it);
  void DisconnectCapturer(CapturerMap::iterator it);

  const std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;

  // These are computed based on the set of currently-available devices.
  std::map<media::audio::RenderUsage, std::vector<GainControlId>> gain_controls_per_render_usage_;
  std::map<media::audio::CaptureUsage, std::vector<GainControlId>> gain_controls_per_capture_usage_;

  // Devices are sorted by plug time.
  std::vector<OutputDevicePipelineInfo> output_devices_;
  std::vector<InputDevicePipelineInfo> input_devices_;

  // Current routes between renderers/capturers and devices.
  RendererMap renderers_;
  CapturerMap capturers_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_ROUTE_GRAPH_H_

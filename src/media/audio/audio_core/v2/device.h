// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_AUDIO_CORE_V2_DEVICE_H_
#define SRC_MEDIA_AUDIO_AUDIO_CORE_V2_DEVICE_H_

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio.device/cpp/wire.h>
#include <fidl/fuchsia.audio.mixer/cpp/wire.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fit/function.h>
#include <lib/zx/time.h>

#include <memory>
#include <utility>

#include "src/media/audio/audio_core/shared/device_config.h"
#include "src/media/audio/audio_core/shared/device_lister.h"
#include "src/media/audio/audio_core/v2/graph_types.h"
#include "src/media/audio/audio_core/v2/input_device_pipeline.h"
#include "src/media/audio/audio_core/v2/output_device_pipeline.h"
#include "src/media/audio/audio_core/v2/reference_clock.h"
#include "src/media/audio/audio_core/v2/route_graph.h"
#include "src/media/audio/lib/effects_loader/effects_loader_v2.h"

namespace media_audio {

// Wraps an (input or output) audio device, along with an {Input,Output}DevicePipeline.
// Notifies the RouteGraph when the device is plugged or unplugged.
class Device : public std::enable_shared_from_this<Device> {
 public:
  using OutputDeviceProfile = media::audio::DeviceConfig::OutputDeviceProfile;
  using InputDeviceProfile = media::audio::DeviceConfig::InputDeviceProfile;

  struct Args {
    // Connection to the mixer service.
    std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client;

    // Connection to ADR.
    fidl::ClientEnd<fuchsia_audio_device::Observer> observer_client;
    fidl::ClientEnd<fuchsia_audio_device::Control> control_client;

    // Dispatcher for managing other FIDL connections.
    async_dispatcher_t* dispatcher;

    // Device properties.
    fuchsia_audio_device::wire::Info info;

    // Device format.
    Format format;

    // Thread on which to run this device's {input,output} pipeline.
    ThreadId thread;

    // Configuration for this device.
    std::variant<OutputDeviceProfile, InputDeviceProfile> config;

    // How many bytes are needed for our end of the ring buffer. For output devices, this is
    // "producer bytes". For input devices, this is "consumer bytes".
    int64_t min_ring_buffer_bytes;

    // For routing this device on plug/unplug.
    std::shared_ptr<RouteGraph> route_graph;

    // For loading effects in OutputDevicePipelines.
    std::shared_ptr<media::audio::EffectsLoaderV2> effects_loader;
  };

  static std::shared_ptr<Device> Create(Args args);

  // Called when the device is removed.
  void Destroy();

  // Public for std::make_shared. Use Create.
  explicit Device(Args args);

 private:
  bool IsOutputPipeline() const;

  void WatchPlugState();
  void UpdateRouteGraph();

  // Initialization sequence.
  void StartRingBuffer();
  void FetchDelayInfo();
  void CreatePipeline();
  void MaybeStartPipeline();
  void MaybeSetGain(std::optional<float> gain_db);

  const std::shared_ptr<fidl::WireSharedClient<fuchsia_audio_mixer::Graph>> graph_client_;
  const fuchsia_audio_device::Info info_;
  const ThreadId thread_;
  const std::variant<OutputDeviceProfile, InputDeviceProfile> config_;
  const std::shared_ptr<RouteGraph> route_graph_;
  const std::shared_ptr<media::audio::EffectsLoaderV2> effects_loader_;
  async_dispatcher_t* const dispatcher_;

  fidl::WireSharedClient<fuchsia_audio_device::Control> control_client_;
  fidl::WireSharedClient<fuchsia_audio_device::Observer> observer_client_;
  fidl::WireSharedClient<fuchsia_audio_device::RingBuffer> ring_buffer_client_;

  // At most one of these is non-nullptr.
  // Both are nullptr during initialization.
  std::shared_ptr<OutputDevicePipeline> output_pipeline_;
  std::shared_ptr<InputDevicePipeline> input_pipeline_;

  // State learned during initialization.
  std::optional<fuchsia_audio::RingBuffer> ring_buffer_;
  std::optional<fuchsia_audio_device::DelayInfo> delay_info_;
  std::optional<zx::time> ring_buffer_start_time_;

  // The time at which the device was last plugged in, or std::nullopt if not plugged in.
  std::optional<zx::time> plug_time_;

  // True iff the device's graph node has been started.
  bool graph_node_started_ = false;

  // True iff this device has been added to `route_graph_`.
  bool routed_ = false;

  // True once the device has been destroyed.
  bool destroyed_ = false;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_AUDIO_CORE_V2_DEVICE_H_

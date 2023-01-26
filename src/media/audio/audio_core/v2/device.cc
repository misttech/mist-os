// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/device.h"

#include <fidl/fuchsia.audio.device/cpp/type_conversions.h>
#include <fidl/fuchsia.audio/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/type_conversions.h>
#include <lib/syslog/cpp/macros.h>

namespace media_audio {

namespace {

template <typename ResultT>
bool LogResultError(const ResultT& result, const char* debug_context) {
  if (!result.ok()) {
    if (!result.is_peer_closed() && !result.is_canceled()) {
      FX_LOGS(WARNING) << debug_context << ": failed with transport error: " << result;
    }
    return true;
  }
  if (!result->is_ok()) {
    FX_LOGS(ERROR) << debug_context
                   << ": failed with code: " << fidl::ToUnderlying(result->error_value());
    return true;
  }
  return false;
}

}  // namespace

// Initialization sequence:
//
// +-------------------------------------------------------------------------------------+
// | Device::Create                                                                      |
// |                 Control.CreateRingBuffer                    Observer.WatchPlugState |
// +--------------------|-----------|------------------------------|---------------------+
//                      |           |                              |
// +--------------------V----+  +---V-----------------------+      | plugged
// | Device::StartRingBuffer |  | Device::FetchDelayInfo    |      |
// | RingBuffer.Start        |  | RingBuffer.WatchDelayInfo |      |
// +-------------------------+  +---------------------------+      |
//                      |           |                              |
//                      |       +---V-----------------------+      |
//                      |       | Device::CreatePipeline    |      |
//                      |       +---------------------------+      |
//                      |           |                              |
//              +-------V-----------V--------+                     |
//              | Device::MaybeStartPipeline |                     |
//              | Graph.Start                |                     |
//              +----------------------------+                     |
//                      |                                          |
//              +-------V------------------+                       |
//              | Device::UpdateRouteGraph |<----------------------+
//              +-------|------------------+
//                      V
//                    routed!

// static
std::shared_ptr<Device> Device::Create(Args args) {
  const auto format = args.format;
  auto device = std::make_shared<Device>(std::move(args));

  fidl::Arena<> arena;

  // Set device gain if requested.
  if (device->IsOutputPipeline()) {
    device->MaybeSetGain(std::get<OutputDeviceProfile>(device->config_).driver_gain_db());
  } else {
    device->MaybeSetGain(std::get<InputDeviceProfile>(device->config_).driver_gain_db());
  }

  // Create the ring buffer.
  auto ring_buffer_endpoints = fidl::CreateEndpoints<fuchsia_audio_device::RingBuffer>();
  if (!ring_buffer_endpoints.is_ok()) {
    FX_PLOGS(FATAL, ring_buffer_endpoints.status_value()) << "fidl::CreateEndpoints failed";
  }

  device->control_client_
      ->CreateRingBuffer(
          fuchsia_audio_device::wire::ControlCreateRingBufferRequest::Builder(arena)
              .options(fuchsia_audio_device::wire::RingBufferOptions::Builder(arena)
                           .format(format.ToWireFidl(arena))
                           .ring_buffer_min_bytes(static_cast<uint32_t>(args.min_ring_buffer_bytes))
                           .Build())
              .ring_buffer_server(std::move(ring_buffer_endpoints->server))
              .Build())
      .Then([device](auto& result) {
        if (!LogResultError(result, "CreateRingBuffer")) {
          return;
        }
        if (!result->value()->has_properties()) {
          FX_LOGS(ERROR) << "CreateRingBuffer bug: response missing `properties`";
          return;
        }
        if (!result->value()->has_ring_buffer()) {
          FX_LOGS(ERROR) << "CreateRingBuffer bug: response missing `ring_buffer`";
          return;
        }
        device->ring_buffer_ = fidl::ToNatural(result->value()->ring_buffer());
        device->StartRingBuffer();
        device->FetchDelayInfo();
      });

  device->ring_buffer_client_ =
      fidl::WireSharedClient(std::move(ring_buffer_endpoints->client), args.dispatcher);

  // Start watching for plug/unplug events.
  device->WatchPlugState();

  return device;
}

Device::Device(Args args)
    : graph_client_(std::move(args.graph_client)),
      info_(fidl::ToNatural(args.info)),
      thread_(args.thread),
      config_(std::move(args.config)),
      route_graph_(std::move(args.route_graph)),
      effects_loader_(std::move(args.effects_loader)),
      dispatcher_(args.dispatcher),
      control_client_(fidl::WireSharedClient(std::move(args.control_client), args.dispatcher)),
      observer_client_(fidl::WireSharedClient(std::move(args.observer_client), args.dispatcher)) {}

void Device::Destroy() {
  destroyed_ = true;

  // Close all client channels.
  observer_client_ = fidl::WireSharedClient<fuchsia_audio_device::Observer>();
  control_client_ = fidl::WireSharedClient<fuchsia_audio_device::Control>();
  ring_buffer_client_ = fidl::WireSharedClient<fuchsia_audio_device::RingBuffer>();

  // Unroute.
  plug_time_ = std::nullopt;
  UpdateRouteGraph();

  if (output_pipeline_) {
    output_pipeline_->Destroy();
    output_pipeline_ = nullptr;
  }
  if (input_pipeline_) {
    input_pipeline_->Destroy();
    input_pipeline_ = nullptr;
  }
}

bool Device::IsOutputPipeline() const {
  return std::holds_alternative<OutputDeviceProfile>(config_);
}

void Device::WatchPlugState() {
  observer_client_->WatchPlugState().Then([this, self = shared_from_this()](auto& result) {
    if (destroyed_ || !LogResultError(result, "WatchPlugState")) {
      return;
    }
    if (!result->value()->has_state() || !result->value()->has_plug_time()) {
      FX_LOGS(ERROR) << "WatchPlugState bug: response missing required field";
      return;
    }

    switch (result->value()->state()) {
      case fuchsia_audio_device::PlugState::kPlugged:
        plug_time_ = zx::time(result->value()->plug_time());
        break;
      case fuchsia_audio_device::PlugState::kUnplugged:
        plug_time_ = std::nullopt;
        break;
      default:
        FX_LOGS(ERROR) << "WatchPlugState unknown state '"
                       << fidl::ToUnderlying(result->value()->state()) << "'";
        break;
    }

    UpdateRouteGraph();
    WatchPlugState();
  });
}

void Device::UpdateRouteGraph() {
  if (plug_time_.has_value() && !routed_) {
    FX_CHECK(graph_node_started_);
    FX_CHECK(!destroyed_);
    // Need to route if a pipeline is available.
    if (output_pipeline_) {
      route_graph_->AddOutputDevice(output_pipeline_, *plug_time_);
      routed_ = true;
    } else if (input_pipeline_) {
      route_graph_->AddInputDevice(input_pipeline_, *plug_time_);
      routed_ = true;
    }
  } else if (!plug_time_.has_value() && routed_) {
    // Need to unroute.
    if (output_pipeline_) {
      route_graph_->RemoveOutputDevice(output_pipeline_);
      routed_ = false;
    } else if (input_pipeline_) {
      route_graph_->RemoveInputDevice(input_pipeline_);
      routed_ = false;
    } else {
      FX_LOGS(FATAL) << "invalid 'routed_' value";
    }
  }
}

void Device::StartRingBuffer() {
  FX_CHECK(ring_buffer_client_);

  fidl::Arena<> arena;
  ring_buffer_client_
      ->Start(fuchsia_audio_device::wire::RingBufferStartRequest::Builder(arena).Build())
      .Then([this, self = shared_from_this()](auto& result) {
        if (destroyed_ || !LogResultError(result, "RingBuffer.Start")) {
          return;
        }
        if (!result->value()->has_start_time()) {
          FX_LOGS(ERROR) << "RingBuffer.Start bug: response missing `start_time`";
          return;
        }
        ring_buffer_start_time_ = zx::time(result->value()->start_time());
        MaybeStartPipeline();
      });
}

void Device::FetchDelayInfo() {
  FX_CHECK(ring_buffer_client_);

  ring_buffer_client_->WatchDelayInfo().Then([this, self = shared_from_this()](auto& result) {
    if (destroyed_ || !LogResultError(result, "RingBuffer.WatchDelayInfo")) {
      return;
    }
    if (!result->value()->has_delay_info() || !result->value()->delay_info().has_internal_delay() ||
        !result->value()->delay_info().has_external_delay()) {
      FX_LOGS(ERROR) << "RingBuffer.WatchDelayInfo bug: response missing required field";
      return;
    }
    delay_info_ = fidl::ToNatural(result->value()->delay_info());
    CreatePipeline();
  });
}

void Device::CreatePipeline() {
  FX_CHECK(delay_info_);
  FX_CHECK(ring_buffer_);
  FX_CHECK(!output_pipeline_);
  FX_CHECK(!input_pipeline_);

  fidl::Arena<> arena;
  auto external_delay_watcher =
      fuchsia_audio_mixer::wire::ExternalDelayWatcher::Builder(arena)
          .initial_delay(*delay_info_->internal_delay() + *delay_info_->external_delay())
          .Build();

  if (IsOutputPipeline()) {
    OutputDevicePipeline::Create({
        .graph_client = graph_client_,
        .dispatcher = dispatcher_,
        .consumer =
            {
                .name = "OutputDevice" + std::to_string(*info_.token_id()),
                .thread = thread_,
                .ring_buffer = fidl::ToWire(arena, std::move(*ring_buffer_)),
                .external_delay_watcher = external_delay_watcher,
            },
        .config = std::get<OutputDeviceProfile>(config_),
        .effects_loader = effects_loader_,
        .callback =
            [this, self = shared_from_this()](auto pipeline) {
              if (destroyed_) {
                return;
              }
              if (!pipeline) {
                FX_LOGS(ERROR) << "Failed to create output pipeline for device "
                               << *info_.token_id();
                return;
              }
              output_pipeline_ = std::move(pipeline);
              MaybeStartPipeline();
            },
    });
  } else {
    InputDevicePipeline::CreateForDevice({
        .graph_client = graph_client_,
        .dispatcher = dispatcher_,
        .producer =
            {
                .name = "InputDevice" + std::to_string(*info_.token_id()),
                .ring_buffer = fidl::ToWire(arena, std::move(*ring_buffer_)),
                .external_delay_watcher = external_delay_watcher,
            },
        .config = std::get<InputDeviceProfile>(config_),
        .thread = thread_,
        .callback =
            [this, self = shared_from_this()](auto pipeline) {
              if (destroyed_) {
                return;
              }
              if (!pipeline) {
                FX_LOGS(ERROR) << "Failed to create input pipeline for device "
                               << *info_.token_id();
                return;
              }
              input_pipeline_ = std::move(pipeline);
              MaybeStartPipeline();
            },
    });
  }

  // This has been consumed by fidl::ToWire (see above).
  ring_buffer_ = std::nullopt;
}

void Device::MaybeStartPipeline() {
  FX_CHECK(!graph_node_started_);
  FX_CHECK(!destroyed_);

  if (!ring_buffer_start_time_ || !delay_info_ || (!output_pipeline_ && !input_pipeline_)) {
    return;
  }

  NodeId node;
  zx::duration stream_time_at_start;
  if (IsOutputPipeline()) {
    // Abstractly, we can think of the hardware buffer as an infinitely long sequence of frames,
    // where the hardware maintains three pointers into this sequence:
    //
    //      |<--- external delay --->|<--- internal delay --->|
    //      +-+----------------------+-+----------------------+-+
    //  ... |P|                      |F|                      |W| ...
    //      +-+----------------------+-+----------------------+-+
    //
    // At any moment:
    // W refers to the frame that is about to be consumed by the device.
    // F refers to the frame that just exited the device's internal pipeline.
    // P refers to the frame being presented to the speaker.
    //
    // In the above diagram, frames move right-to-left over time:
    // - "Internal delay" is the time needed for a frame to move from position W to position F;
    // - "External delay" is the time needed for a frame to move from position F to position P.
    //
    // The very first frame of this sequence is frame 0, a.k.a. `stream_time = 0`. This frame is
    // written to offset 0 of the ring buffer. When the ring buffer starts, F points at frame 0.
    // Hence, at the moment the ring buffer starts, it conceptually plays the frame at `stream_time
    // = -external_delay`. (In practice, nothing is played immediately when the device starts;
    // instead the device is silent for the first "external delay", after which frame 0 becomes the
    // first frame presented at the speaker.)
    FX_CHECK(output_pipeline_);
    node = output_pipeline_->consumer_node();
    stream_time_at_start = -zx::nsec(*delay_info_->external_delay());
  } else {
    // The capture buffer works in a similar way, with three analogous pointers:
    //
    //        |<--- internal delay --->|<--- external delay --->|
    //      +-+----------------------+-+----------------------+-+
    //  ... |R|                      |F|                      |C| ...
    //      +-+----------------------+-+----------------------+-+
    //
    // At any moment:
    // R refers to the frame just written to the ring buffer, newly available to capture clients.
    // F refers to the frame just emitted by the interconnect, entering any internal pipeline.
    // C refers to the frame currently being captured by the microphone.
    //
    // As with playback, in our diagram any specific frame will move right-to-left over time:
    // - "External delay" is the time needed for a frame to move from position C to position F;
    // - "Internal delay" is the time needed for a frame to move from position F to position R.
    //
    // Just as with playback, F points at frame 0 at the moment the ring buffer starts. Hence, at
    // the moment the ring buffer starts, offset 0 of the ring buffer contains the frame that was
    // (conceptually) captured "external delay" ago.
    FX_CHECK(input_pipeline_);
    FX_CHECK(input_pipeline_->producer_node());
    node = *input_pipeline_->producer_node();
    stream_time_at_start = -zx::nsec(*delay_info_->external_delay());
  }

  fidl::Arena<> arena;
  (*graph_client_)
      ->Start(fuchsia_audio_mixer::wire::GraphStartRequest::Builder(arena)
                  .node_id(node)
                  .when(fuchsia_media2::wire::RealTime::WithReferenceTime(
                      arena, ring_buffer_start_time_->get()))
                  .stream_time(fuchsia_media2::wire::StreamTime::WithStreamTime(
                      arena, stream_time_at_start.get()))
                  .Build())
      .Then([this, self = shared_from_this()](auto& result) {
        if (destroyed_ || !LogResultError(result, "Graph.Start")) {
          return;
        }
        graph_node_started_ = true;
        UpdateRouteGraph();
      });
}

void Device::MaybeSetGain(std::optional<float> gain_db) {
  if (!gain_db) {
    return;
  }

  fidl::Arena<> arena;
  control_client_
      ->SetGain(
          fuchsia_audio_device::wire::ControlSetGainRequest::Builder(arena)
              .target_state(
                  fuchsia_audio_device::wire::GainState::Builder(arena).gain_db(*gain_db).Build())
              .Build())
      .Then([this, self = shared_from_this()](auto& result) {
        if (!destroyed_) {
          LogResultError(result, "SetGain");
        }
      });
}

}  // namespace media_audio

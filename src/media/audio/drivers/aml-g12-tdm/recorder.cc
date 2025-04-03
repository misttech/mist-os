// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/recorder.h"

#include <lib/inspect/cpp/vmo/types.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>

#include "src/media/audio/drivers/aml-g12-tdm/composite-server.h"

namespace audio::aml_g12 {

PowerTransition::PowerTransition(inspect::Node node, bool state, const zx::time& called_at,
                                 const zx::time& completed_at)
    : node_(std::move(node)) {
  state_ = node_.CreateBool(kPowerState, state);
  called_at_ = node_.CreateInt(kCalledAt, called_at.get());
  completed_at_ = node_.CreateInt(kEffectiveAt, completed_at.get());
}

DaiEntry::DaiEntry(inspect::Node node, uint64_t element_id) : node_(std::move(node)) {
  element_id_ = node_.CreateUint(kElementId, element_id);
}

ActiveChannelsCall::ActiveChannelsCall(inspect::Node node, uint64_t channel_mask,
                                       const zx::time& called_at, const zx::time& completed_at)
    : node_(std::move(node)) {
  channel_mask_ = node_.CreateUint(kChannelBitmask, channel_mask);
  called_at_ = node_.CreateInt(kCalledAt, called_at.get());
  completed_at_ = node_.CreateInt(kEffectiveAt, completed_at.get());
}

RunningInterval::RunningInterval(inspect::Node node, const zx::time& started_at)
    : node_(std::move(node)) {
  started_at_ = node_.CreateInt(kStartedAt, started_at.get());
}
void RunningInterval::RecordStopTime(const zx::time& stopped_at) {
  stopped_at_ = node_.CreateInt(kStoppedAt, stopped_at.get());
}

RingBufferRecorder::RingBufferRecorder(inspect::Node node, const zx::time& created_at)
    : instance_node_(std::move(node)) {
  created_at_ = instance_node_.CreateInt(kCtorTime, created_at.get());
  active_channels_calls_root_ = instance_node_.CreateChild(kSetActiveChannelsCalls);
  running_intervals_root_ = instance_node_.CreateChild(kRunningIntervals);
}

void RingBufferRecorder::RecordDestructionTime(const zx::time& destroyed_at) {
  destroyed_at_ = instance_node_.CreateInt(kDtorTime, destroyed_at.get());
}

void RingBufferRecorder::RecordStartTime(const zx::time& started_at) {
  RunningInterval running_interval{
      running_intervals_root_.CreateChild(std::to_string(running_intervals_.size())), started_at};
  running_intervals_.emplace_back(std::move(running_interval));
}
void RingBufferRecorder::RecordStopTime(const zx::time& stopped_at) {
  // It's pointless for clients to call Stop before Start, but we shouldn't crash if they do.
  if (!running_intervals_.empty()) {
    running_intervals_.rbegin()->RecordStopTime(stopped_at);
  }
}

void RingBufferRecorder::RecordActiveChannelsCall(uint64_t active_channels_bitmask,
                                                  const zx::time& set_active_channels_called_at,
                                                  const zx::time& active_channels_time_complete) {
  ActiveChannelsCall active_channels_call{
      active_channels_calls_root_.CreateChild(std::to_string(active_channels_calls_.size())),
      active_channels_bitmask, set_active_channels_called_at, active_channels_time_complete};
  active_channels_calls_.emplace_back(std::move(active_channels_call));
}

RingBufferSpecification::RingBufferSpecification(inspect::Node node, uint64_t element_id,
                                                 bool supports_active_channels, bool outgoing)
    : node_(std::move(node)) {
  element_id_ = node_.CreateUint(kElementId, element_id);
  supports_active_channels_ = node_.CreateBool(kSupportsActiveChannels, supports_active_channels);
  outgoing_ = node_.CreateBool(kIsOutgoingStream, outgoing);
}

Recorder::Recorder(inspect::Node& inspect_root) : inspect_root_(inspect_root) {
  PopulateInspectNodes();
}

// This method reaches into AudioCompositeServer and knows "a priori" which TDM engines are
// outgoing, so this isn't a perfect abstraction.
void Recorder::PopulateInspectNodes() {
  current_power_state_ = inspect_root_.CreateBool(kCurrentPowerState, true);
  power_transitions_node_ = inspect_root_.CreateChild(kPowerTransitions);

  ring_buffers_root_node_ = inspect_root_.CreateChild(kRingBuffers);
  for (size_t idx = 0u; idx < kNumberOfTdmEngines; ++idx) {
    auto ring_buffer_spec_node =
        ring_buffers_root_node_.CreateChild("tdm engine #" + std::to_string(idx));
    RingBufferSpecification ring_buffer_spec{std::move(ring_buffer_spec_node),
                                             AudioCompositeServer::kRingBufferIds[idx],
                                             /* supports_active_channels= */ true,
                                             /* outgoing= */ idx < 3};
    ring_buffer_specs_.emplace_back(std::move(ring_buffer_spec));
  }

  dai_root_node_ = inspect_root_.CreateChild(kDAIs);
  for (size_t idx = 0u; idx < kNumberOfPipelines; ++idx) {
    auto dai_node = dai_root_node_.CreateChild("pipeline #" + std::to_string(idx));
    DaiEntry dai_entry{std::move(dai_node), AudioCompositeServer::kDaiIds[idx]};
    dai_entries_.emplace_back(std::move(dai_entry));
  }
}

void Recorder::RecordSocPowerUp(const zx::time& called_at, const zx::time& completed_at) {
  current_power_state_.Set(true);
  power_transitions_.emplace_back(
      power_transitions_node_.CreateChild(std::to_string(power_transitions_.size())), true,
      called_at, completed_at);
}
void Recorder::RecordSocPowerDown(const zx::time& called_at, const zx::time& completed_at) {
  current_power_state_.Set(false);
  power_transitions_.emplace_back(
      power_transitions_node_.CreateChild(std::to_string(power_transitions_.size())), false,
      called_at, completed_at);
}

}  // namespace audio::aml_g12

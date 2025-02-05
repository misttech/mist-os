// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_RECORDER_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_RECORDER_H_

#include <lib/inspect/cpp/vmo/types.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>

namespace audio::aml_g12 {

//
// Recorder class and subclasses
// The Recorder class is responsible for creating and updating Inspect -- so we don't need to
// implement this functionality in classes dedicated to other functions.
//

// Use StringReferences to save space in the Inspect VMO.
static constexpr std::string_view kCurrentPowerState = "current_power_state";

static constexpr std::string_view kPowerTransitions = "power_transitions";
static constexpr std::string_view kCalledAt = "called_at";
static constexpr std::string_view kEffectiveAt = "effective_at";
static constexpr std::string_view kPowerState = "power_state";

static constexpr std::string_view kRingBuffers = "RingBuffers";
static constexpr std::string_view kElementId = "element_id";
static constexpr std::string_view kSupportsActiveChannels = "supports_active_channels";
static constexpr std::string_view kIsOutgoingStream = "is_outgoing_stream";
static constexpr std::string_view kCtorTime = "ctor_time";
static constexpr std::string_view kDtorTime = "dtor_time";
static constexpr std::string_view kRunningIntervals = "running_intervals";
static constexpr std::string_view kStartedAt = "started_at";
static constexpr std::string_view kStoppedAt = "stopped_at";
static constexpr std::string_view kSetActiveChannelsCalls = "SetActiveChannels_calls";
static constexpr std::string_view kChannelBitmask = "channel_bitmask";

static constexpr std::string_view kDAIs = "DAIs";

// Represents a single power transition.
class PowerTransition {
 public:
  PowerTransition(inspect::Node node, bool state, const zx::time& called_at,
                  const zx::time& completed_at);

 private:
  inspect::Node node_;
  inspect::BoolProperty state_;
  inspect::IntProperty called_at_;
  inspect::IntProperty completed_at_;
};

// Represents the specification (unchanging information) of a Dai element.
class DaiEntry {
 public:
  DaiEntry(inspect::Node node, uint64_t element_id);
  inspect::Node& node() { return node_; }

 private:
  inspect::Node node_;
  inspect::UintProperty element_id_;
};

// Represents a call to SetActiveChannels.
class ActiveChannelsCall {
 public:
  ActiveChannelsCall(inspect::Node node, uint64_t channel_mask, const zx::time& called_at,
                     const zx::time& completed_at);

 private:
  inspect::Node node_;
  inspect::UintProperty channel_mask_;
  inspect::IntProperty called_at_;
  inspect::IntProperty completed_at_;
};

// Represents an interval during which a RingBuffer instance is started.
class RunningInterval {
 public:
  RunningInterval(inspect::Node node, const zx::time& started_at);
  void RecordStopTime(const zx::time& stopped_at);

 private:
  inspect::Node node_;
  inspect::IntProperty started_at_;
  inspect::IntProperty stopped_at_;
};

// One of the primary classes used by an outside class.
// Records info about a ring buffer instance, such as lifetime, start/stop, SetActiveChannels.
class RingBufferRecorder {
 public:
  RingBufferRecorder(inspect::Node node, const zx::time& created_at);

  void RecordDestructionTime(const zx::time& destroyed_at);

  void RecordStartTime(const zx::time& started_at);
  void RecordStopTime(const zx::time& stopped_at);

  void RecordActiveChannelsCall(uint64_t active_channels_bitmask, const zx::time& called_at,
                                const zx::time& completed_at);

 private:
  inspect::Node instance_node_;
  inspect::IntProperty created_at_;
  inspect::IntProperty destroyed_at_;

  inspect::Node active_channels_calls_root_;
  std::vector<ActiveChannelsCall> active_channels_calls_;

  inspect::Node running_intervals_root_;
  std::vector<RunningInterval> running_intervals_;
};

// Represents the specification (unchanging information) of a RingBuffer element.
class RingBufferSpecification {
 public:
  RingBufferSpecification(inspect::Node node, uint64_t element_id, bool supports_active_channels,
                          bool outgoing);
  inspect::Node& node() { return node_; }
  std::vector<RingBufferRecorder>& instances() { return ring_buffer_inspect_instances_; }

 private:
  inspect::Node node_;
  inspect::UintProperty element_id_;
  inspect::BoolProperty supports_active_channels_;
  inspect::BoolProperty outgoing_;
  std::vector<RingBufferRecorder> ring_buffer_inspect_instances_;
};

// One of the primary classes used by an outside class.
// Records info about a device, such as lifetime, power transitions, and Dai/RingBuffer elements.
class Recorder final {
 public:
  explicit Recorder(inspect::Node& inspect_root);
  void PopulateInspectNodes();

  void RecordSocPowerUp(const zx::time& called_at, const zx::time& completed_at);
  void RecordSocPowerDown(const zx::time& called_at, const zx::time& completed_at);

  RingBufferRecorder& CreateRingBufferInstance(size_t ring_buffer_specification_index,
                                               const zx::time& created_at) {
    std::string node_name =
        "instance " +
        std::to_string(ring_buffer_specs_[ring_buffer_specification_index].instances().size());

    return ring_buffer_specs_[ring_buffer_specification_index].instances().emplace_back(
        ring_buffer_specs_[ring_buffer_specification_index].node().CreateChild(node_name),
        created_at);
  }

 private:
  inspect::Node& inspect_root_;
  inspect::BoolProperty current_power_state_;

  inspect::Node power_transitions_node_;
  std::vector<PowerTransition> power_transitions_;

  inspect::Node ring_buffers_root_node_;
  std::vector<RingBufferSpecification> ring_buffer_specs_;

  inspect::Node dai_root_node_;
  std::vector<DaiEntry> dai_entries_;
};

}  // namespace audio::aml_g12

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_RECORDER_H_

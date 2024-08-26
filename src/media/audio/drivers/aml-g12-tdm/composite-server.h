// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.clock/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include "src/media/audio/drivers/aml-g12-tdm/aml-tdm-config-device.h"

namespace audio::aml_g12 {

constexpr size_t kNumberOfPipelines = 3;
constexpr size_t kNumberOfTdmEngines = 2 * kNumberOfPipelines;  // 2, 1 for input 1 for output.

class RingBufferServer;
class AudioCompositeServer;

struct Engine {
  size_t ring_buffer_index;
  size_t dai_index;
  std::optional<AmlTdmConfigDevice> device;
  std::unique_ptr<RingBufferServer> ring_buffer;
  fuchsia_hardware_audio::Format ring_buffer_format;
  metadata::AmlConfig config;
};

class RingBufferServer : public fidl::Server<fuchsia_hardware_audio::RingBuffer> {
 public:
  static std::unique_ptr<RingBufferServer> CreateRingBufferServer(
      async_dispatcher_t* dispatcher, AudioCompositeServer& owner, size_t engine_index,
      fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> ring_buffer);
  RingBufferServer(async_dispatcher_t* dispatcher, AudioCompositeServer& owner, size_t engine_index,
                   fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> ring_buffer);
  void Unbind(zx_status_t status) { binding_.Close(status); }

 protected:
  // FIDL natural C++ methods for fuchsia.hardware.audio.RingBuffer.
  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void GetVmo(
      GetVmoRequest& request,
      fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetVmoCompleter::Sync& completer) override;

  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void WatchClockRecoveryPositionInfo(
      WatchClockRecoveryPositionInfoCompleter::Sync& completer) override;
  void WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) override;
  void SetActiveChannels(fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
                         SetActiveChannelsCompleter::Sync& completer) override;

 private:
  void OnRingBufferClosed(fidl::UnbindInfo info);
  zx_status_t InitBuffer(size_t size);
  void ProcessRingNotification();
  void ResetRingBuffer();

  Engine& engine_;
  size_t engine_index_;
  async_dispatcher_t* dispatcher_;
  AudioCompositeServer& owner_;
  fidl::ServerBinding<fuchsia_hardware_audio::RingBuffer> binding_;

  zx::duration notification_period_{0};
  bool started_ = false;
  bool fetched_ = false;
  zx::duration internal_delay_{0};

  async::TaskClosureMethod<RingBufferServer, &RingBufferServer::ProcessRingNotification>
      notify_timer_{this};
  zx::vmo ring_buffer_vmo_;
  fzl::PinnedVmo pinned_ring_buffer_;
  std::atomic<uint32_t> expected_notifications_per_ring_{0};

  std::optional<WatchClockRecoveryPositionInfoCompleter::Async> position_completer_;
  struct DelayCompleter {
    // One-shot flag that indicates whether or not WatchDelayInfo has been called yet.
    bool first_response_sent;
    std::optional<WatchDelayInfoCompleter::Async> completer;
  };
  DelayCompleter delay_completer_;

  // Inspect-related
  //
  void PopulateInstanceNode(size_t ring_buffer_index);

  class ActiveChannelsCall {
   public:
    ActiveChannelsCall(inspect::Node node, uint64_t channel_mask, zx_time_t call_time,
                       zx_time_t completion_time)
        : node_(std::move(node)) {
      channel_mask_ = node_.CreateUint("channel bitmask", channel_mask);
      call_time_ = node_.CreateUint("called at      ", call_time);
      completion_time_ = node_.CreateUint("effective at   ", completion_time);
    }
    inspect::Node& node() { return node_; }

   private:
    inspect::Node node_;
    inspect::UintProperty channel_mask_;
    inspect::UintProperty call_time_;
    inspect::UintProperty completion_time_;
  };
  std::vector<ActiveChannelsCall> active_channels_calls_;

  class RunningInterval {
   public:
    RunningInterval(inspect::Node node, zx_time_t start_time) : node_(std::move(node)) {
      start_time_ = node_.CreateUint("started at", start_time);
      stop_time_ = node_.CreateUint("stopped at", 0);
    }
    inspect::Node& node() { return node_; }
    void SetStopTime(zx_time_t stop_time) { stop_time_.Set(stop_time); }

   private:
    inspect::Node node_;
    inspect::UintProperty start_time_;
    inspect::UintProperty stop_time_;
  };
  std::vector<RunningInterval> running_intervals_;

  // updaters
  void RecordDestructionTime(zx_time_t destruction_time) { destroyed_at_.Set(destruction_time); }

  void RecordStartTime(zx_time_t start_time) {
    RunningInterval running_interval{
        running_intervals_root_.CreateChild(std::to_string(running_intervals_.size())), start_time};
    running_intervals_.emplace_back(std::move(running_interval));
  }
  void RecordStopTime(zx_time_t stop_time) { running_intervals_.rbegin()->SetStopTime(stop_time); }

  void RecordActiveChannelsCall(uint64_t active_channels_bitmask,
                                zx_time_t set_active_channels_call_time,
                                zx_time_t active_channels_time_complete) {
    ActiveChannelsCall active_channels_call{
        active_channels_calls_root_.CreateChild(std::to_string(active_channels_calls_.size())),
        active_channels_bitmask, set_active_channels_call_time, active_channels_time_complete};
    active_channels_calls_.emplace_back(std::move(active_channels_call));
  }

  inspect::Node instance_node_;
  inspect::Node active_channels_calls_root_;
  inspect::Node running_intervals_root_;
  inspect::IntProperty created_at_;
  inspect::IntProperty started_at_;
  inspect::IntProperty set_active_channels_called_at_;
  inspect::UintProperty current_active_channels_bitmask_;
  inspect::IntProperty set_active_channels_completed_at_;
  inspect::IntProperty stopped_at_;
  inspect::IntProperty destroyed_at_;
};

class AudioCompositeServer
    : public fidl::Server<fuchsia_hardware_audio::Composite>,
      public fidl::Server<fuchsia_hardware_audio_signalprocessing::SignalProcessing> {
  friend class RingBufferServer;

 public:
  AudioCompositeServer(
      std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios, zx::bti bti,
      async_dispatcher_t* dispatcher, metadata::AmlVersion aml_version,
      fidl::WireSyncClient<fuchsia_hardware_clock::Clock> clock_gate_client,
      fidl::WireSyncClient<fuchsia_hardware_clock::Clock> pll_client,
      std::vector<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpio_sclk_clients,
      inspect::Node& inspect_root);

  async_dispatcher_t* dispatcher() { return dispatcher_; }
  zx::bti& bti() { return bti_; }
  fuchsia_hardware_audio::DaiFormat& current_dai_formats(size_t dai_index) {
    ZX_ASSERT(current_dai_formats_[dai_index].has_value());
    return *current_dai_formats_[dai_index];
  }
  size_t next_ring_buffer_instance_index(size_t engine_index) {
    return ring_buffer_instance_count_[engine_index]++;
  }

 protected:
  // FIDL natural C++ methods for fuchsia.hardware.audio.Composite.
  void Reset(ResetCompleter::Sync& completer) override;
  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override;
  void SignalProcessingConnect(SignalProcessingConnectRequest& request,
                               SignalProcessingConnectCompleter::Sync& completer) override;
  void GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                            GetRingBufferFormatsCompleter::Sync& completer) override;
  void CreateRingBuffer(CreateRingBufferRequest& request,
                        CreateRingBufferCompleter::Sync& completer) override;
  void GetDaiFormats(GetDaiFormatsRequest& request,
                     GetDaiFormatsCompleter::Sync& completer) override;
  void SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) override;

  // FIDL natural C++ methods for fuchsia.hardware.audio.signalprocessing.SignalProcessing.
  void GetElements(GetElementsCompleter::Sync& completer) override;
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) override;
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) override;
  void GetTopologies(GetTopologiesCompleter::Sync& completer) override;
  void WatchTopology(WatchTopologyCompleter::Sync& completer) override;
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) override;

 private:
  static constexpr std::array<fuchsia_hardware_audio::ElementId, kNumberOfPipelines> kDaiIds = {
      1, 2, 3};
  static constexpr std::array<fuchsia_hardware_audio::ElementId, kNumberOfTdmEngines>
      kRingBufferIds = {4, 5, 6, 7, 8, 9};
  static constexpr fuchsia_hardware_audio::TopologyId kTopologyId = 1;

  static constexpr zx::duration kTimeToStabilizePll = zx::msec(10);

  struct ElementCompleter {
    // One-shot flag that indicates whether or not WatchElementState has been called
    // for this element yet.
    bool first_response_sent;
    std::optional<WatchElementStateCompleter::Async> completer;
  };
  struct TopologyCompleter {
    // One-shot flag that indicates whether or not WatchTopology has been called
    // for this topology yet.
    bool first_response_sent;
    std::optional<WatchTopologyCompleter::Async> completer;
  };

  void OnSignalProcessingClosed(fidl::UnbindInfo info);
  zx_status_t ResetEngine(size_t index);
  zx_status_t ConfigEngine(size_t index, size_t dai_index, bool input, fdf::MmioBuffer mmio,
                           metadata::AmlVersion aml_version);
  zx_status_t StartSocPower(bool wait_for_completion);
  zx_status_t StopSocPower();

  async_dispatcher_t* dispatcher_;
  zx::bti bti_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      signal_;

  TopologyCompleter topology_completer_ = {};

  std::unordered_map<fuchsia_hardware_audio::ElementId, ElementCompleter> element_completers_;
  std::array<Engine, kNumberOfTdmEngines> engines_;
  std::bitset<kNumberOfTdmEngines> engines_on_;
  std::array<fuchsia_hardware_audio::PcmSupportedFormats, kNumberOfTdmEngines>
      supported_ring_buffer_formats_;
  std::array<fuchsia_hardware_audio::DaiSupportedFormats, kNumberOfPipelines>
      supported_dai_formats_;
  std::array<std::optional<fuchsia_hardware_audio::DaiFormat>, kNumberOfPipelines>
      current_dai_formats_;

  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> clock_gate_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> pll_;
  std::vector<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpio_sclk_clients_;
  bool soc_power_started_ = false;
  zx::time last_started_time_;
  zx::time last_stopped_time_;
  trace_async_id_t trace_async_id_;

  // Inspect-related
  //
  class PowerTransition {
   public:
    PowerTransition(inspect::Node node, bool state, const zx::time& call_time,
                    const zx::time& completion_time)
        : node_(std::move(node)) {
      state_ = node_.CreateBool("power state ", state);
      call_time_ = node_.CreateUint("called at   ", call_time.get());
      completion_time_ = node_.CreateUint("effective at", completion_time.get());
    }

   private:
    inspect::Node node_;
    inspect::BoolProperty state_;
    inspect::UintProperty call_time_;
    inspect::UintProperty completion_time_;
  };

  class RingBufferSpecification {
   public:
    RingBufferSpecification(inspect::Node node, uint64_t element_id, bool supports_active_channels,
                            bool outgoing)
        : node_(std::move(node)) {
      element_id_ = node_.CreateUint("element id           ", element_id);
      supports_active_channels_ =
          node_.CreateBool("supports active_chans", supports_active_channels);
      outgoing_ = node_.CreateBool("is outgoing stream   ", outgoing);
    }
    inspect::Node& node() { return node_; }

   private:
    inspect::Node node_;
    inspect::UintProperty element_id_;
    inspect::BoolProperty supports_active_channels_;
    inspect::BoolProperty outgoing_;
  };

  class DaiEntry {
   public:
    DaiEntry(inspect::Node node, uint64_t element_id) : node_(std::move(node)) {
      element_id_ = node_.CreateUint("element id", element_id);
    }
    inspect::Node& node() { return node_; }

   private:
    inspect::Node node_;
    inspect::UintProperty element_id_;
  };

  void PopulateInspectNodes();

  // updaters
  void RecordSocPowerUp(const zx::time& call_time, const zx::time& completion_time) {
    current_power_state_.Set(true);
    power_transitions_.emplace_back(
        power_transitions_node_.CreateChild(std::to_string(power_transitions_.size())), true,
        call_time, completion_time);
  }
  void RecordSocPowerDown(const zx::time& call_time, const zx::time& completion_time) {
    current_power_state_.Set(false);
    power_transitions_.emplace_back(
        power_transitions_node_.CreateChild(std::to_string(power_transitions_.size())), false,
        call_time, completion_time);
  }

  inspect::Node& inspect_root_;
  inspect::BoolProperty current_power_state_;

  inspect::Node dai_root_node_;
  std::vector<DaiEntry> dai_entries_;

  inspect::Node ring_buffers_root_node_;
  std::vector<RingBufferSpecification> ring_buffer_specs_;
  std::array<size_t, kNumberOfTdmEngines> ring_buffer_instance_count_{0};

  inspect::Node power_transitions_node_;
  std::vector<PowerTransition> power_transitions_;
};

}  // namespace audio::aml_g12

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_
#define SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.clock/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fzl/pinned-vmo.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include <unordered_map>

#include "src/media/audio/drivers/aml-g12-tdm/aml-tdm-config-device.h"

namespace audio::aml_g12 {

constexpr size_t kNumberOfPipelines = 3;
constexpr size_t kNumberOfTdmEngines = 2 * kNumberOfPipelines;  // 2, 1 for input 1 for output.

class RingBufferServer;
class AudioCompositeServer;

struct Engine {
  size_t dai_index;
  std::optional<AmlTdmConfigDevice> device;
  std::unique_ptr<RingBufferServer> ring_buffer;
  metadata::AmlConfig config;
};

class RingBufferServer : public fidl::Server<fuchsia_hardware_audio::RingBuffer> {
 public:
  static std::unique_ptr<RingBufferServer> CreateRingBufferServer(
      async_dispatcher_t* dispatcher, AudioCompositeServer& owner, Engine& engine,
      fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> ring_buffer);
  RingBufferServer(async_dispatcher_t* dispatcher, AudioCompositeServer& owner, Engine& engine,
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
};

class AudioCompositeServer
    : public fidl::Server<fuchsia_hardware_audio::Composite>,
      public fidl::Server<fuchsia_hardware_audio_signalprocessing::SignalProcessing> {
 public:
  // Cast is needed because PowerLevel and BinaryPowerLevel are distinct types.
  static const fuchsia_power_broker::PowerLevel kAudioHardwareOn =
      static_cast<fuchsia_power_broker::PowerLevel>(fuchsia_power_broker::BinaryPowerLevel::kOn);
  static const fuchsia_power_broker::PowerLevel kAudioHardwareOff =
      static_cast<fuchsia_power_broker::PowerLevel>(fuchsia_power_broker::BinaryPowerLevel::kOff);

  AudioCompositeServer(
      std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios, zx::bti bti,
      async_dispatcher_t* dispatcher, metadata::AmlVersion aml_version,
      fidl::WireSyncClient<fuchsia_hardware_clock::Clock> clock_gate_client,
      fidl::WireSyncClient<fuchsia_hardware_clock::Clock> pll_client,
      std::vector<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpio_sclk_clients,
      fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control,
      fidl::SyncClient<fuchsia_power_broker::Lessor> lessor,
      fidl::SyncClient<fuchsia_power_broker::CurrentLevel> current_level,
      fidl::Client<fuchsia_power_broker::RequiredLevel> required_level);

  async_dispatcher_t* dispatcher() { return dispatcher_; }
  zx::bti& bti() { return bti_; }
  fuchsia_hardware_audio::DaiFormat& current_dai_formats(size_t dai_index) {
    ZX_ASSERT(current_dai_formats_[dai_index].has_value());
    return *current_dai_formats_[dai_index];
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
  static constexpr std::array<uint64_t, kNumberOfPipelines> kDaiIds = {1, 2, 3};
  static constexpr std::array<uint64_t, kNumberOfTdmEngines> kRingBufferIds = {4, 5, 6, 7, 8, 9};
  static constexpr uint64_t kTopologyId = 1;

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
  zx_status_t StartSocPower();
  zx_status_t StopSocPower();
  void WatchRequiredLevel();

  async_dispatcher_t* dispatcher_;
  zx::bti bti_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      signal_;

  TopologyCompleter topology_completer_ = {};

  std::unordered_map<uint64_t, ElementCompleter> element_completers_;
  std::array<Engine, kNumberOfTdmEngines> engines_;
  std::array<fuchsia_hardware_audio::PcmSupportedFormats, kNumberOfTdmEngines>
      supported_ring_buffer_formats_;
  std::array<std::optional<fuchsia_hardware_audio::Format>, kNumberOfTdmEngines>
      current_ring_buffer_formats_;
  std::array<fuchsia_hardware_audio::DaiSupportedFormats, kNumberOfPipelines>
      supported_dai_formats_;
  std::array<std::optional<fuchsia_hardware_audio::DaiFormat>, kNumberOfPipelines>
      current_dai_formats_;

  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> clock_gate_;
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> pll_;
  std::vector<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpio_sclk_clients_;
  bool soc_power_started_ = false;
  // Need to keep the ElementControl channel end (returned from adding the power element audio-hw)
  // so the power element stays in the topology.
  fidl::ClientEnd<fuchsia_power_broker::ElementControl> element_control_;
  // This Lessor client allows us to request a lease on the audio-hw power element.
  fidl::SyncClient<fuchsia_power_broker::Lessor> lessor_;
  // We need to keep the LeaseControl channel end to keep the lease on audio-hw.
  fidl::ClientEnd<fuchsia_power_broker::LeaseControl> lease_control_;
  // FIDL client used to monitor the power level to be set as required by the Power Broker.
  fidl::Client<fuchsia_power_broker::RequiredLevel> required_level_;
  // FIDL client used to update the Power Broker about the level set in the hardware.
  fidl::SyncClient<fuchsia_power_broker::CurrentLevel> current_level_;
  trace_async_id_t trace_async_id_;
};

}  // namespace audio::aml_g12

#endif  // SRC_MEDIA_AUDIO_DRIVERS_AML_G12_TDM_COMPOSITE_SERVER_H_

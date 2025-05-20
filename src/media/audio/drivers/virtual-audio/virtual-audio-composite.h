// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_COMPOSITE_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_COMPOSITE_H_

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.virtualaudio/cpp/fidl.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/node/cpp/add_child.h>
#include <lib/fzl/vmo-mapper.h>

namespace virtual_audio {

class VirtualAudioComposite
    : public fidl::Server<fuchsia_virtualaudio::Device>,
      public fidl::Server<fuchsia_hardware_audio::Composite>,
      public fidl::Server<fuchsia_hardware_audio_signalprocessing::SignalProcessing>,
      public fidl::Server<fuchsia_hardware_audio::RingBuffer> {
 public:
  using InstanceId = uint64_t;
  using OnDeviceBindingClosed = fit::callback<void(fidl::UnbindInfo)>;

  static constexpr std::string_view kClassName = "audio-composite";

  static fuchsia_virtualaudio::Configuration GetDefaultConfig();

  static zx::result<std::unique_ptr<VirtualAudioComposite>> Create(
      InstanceId instance_id, fuchsia_virtualaudio::Configuration config,
      async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_virtualaudio::Device> server,
      OnDeviceBindingClosed on_binding_closed,
      fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent);

  VirtualAudioComposite(InstanceId instance_id, fuchsia_virtualaudio::Configuration config,
                        async_dispatcher_t* dispatcher,
                        fidl::ServerEnd<fuchsia_virtualaudio::Device> server,
                        OnDeviceBindingClosed on_device_binding_closed)
      : config_(std::move(config)),
        dispatcher_(dispatcher),
        device_binding_(dispatcher_, std::move(server), this, std::move(on_device_binding_closed)),
        instance_id_(instance_id) {}

  zx::result<> Init(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent);

 private:
  static constexpr size_t kNumberOfElements = 3;
  static constexpr fuchsia_hardware_audio_signalprocessing::ElementId kRingBufferId = 123;
  static constexpr fuchsia_hardware_audio_signalprocessing::ElementId kGainId = 321;
  static constexpr fuchsia_hardware_audio_signalprocessing::ElementId kDaiId = 456;

  static constexpr size_t kNumberOfTopologies = 2;
  // This topology is RingBuffer (123) -> Gain (321) -> Dai        (456)
  static constexpr fuchsia_hardware_audio_signalprocessing::TopologyId kPlaybackTopologyId = 789;
  // This topology is Dai        (456) -> Gain (321) -> RingBuffer (123)
  static constexpr fuchsia_hardware_audio_signalprocessing::TopologyId kCaptureTopologyId = 987;

  // virtualaudio.Device implementation.
  void GetFormat(GetFormatCompleter::Sync& completer) override;
  void GetGain(GetGainCompleter::Sync& completer) override;
  void GetBuffer(GetBufferCompleter::Sync& completer) override;
  void SetNotificationFrequency(SetNotificationFrequencyRequest& request,
                                SetNotificationFrequencyCompleter::Sync& completer) override;
  void GetPosition(GetPositionCompleter::Sync& completer) override;
  void ChangePlugState(ChangePlugStateRequest& request,
                       ChangePlugStateCompleter::Sync& completer) override;
  void AdjustClockRate(AdjustClockRateRequest& request,
                       AdjustClockRateCompleter::Sync& completer) override;

  // fuchsia.hardware.audio.Composite implementation.
  void Reset(ResetCompleter::Sync& completer) override;
  void GetProperties(fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync&
                         completer) override;
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

  // fuchsia.hardware.audio.RingBuffer implementation.
  void GetProperties(fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync&
                         completer) override;
  void GetVmo(GetVmoRequest& request, GetVmoCompleter::Sync& completer) override;
  void Start(StartCompleter::Sync& completer) override;
  void Stop(StopCompleter::Sync& completer) override;
  void WatchClockRecoveryPositionInfo(
      WatchClockRecoveryPositionInfoCompleter::Sync& completer) override;
  void WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) override;
  void SetActiveChannels(SetActiveChannelsRequest& request,
                         SetActiveChannelsCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_audio::RingBuffer> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fuchsia.hardware.audio.signalprocessing implementation (SignalProcessing and Reader).
  void GetElements(GetElementsCompleter::Sync& completer) override;
  void GetTopologies(GetTopologiesCompleter::Sync& completer) override;
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) override;
  void WatchTopology(WatchTopologyCompleter::Sync& completer) override;
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) override;
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_audio_signalprocessing::SignalProcessing>
          metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  void SetupSignalProcessing();
  void SetupSignalProcessingElements();
  void SetupSignalProcessingTopologies();
  void SetupSignalProcessingElementStates();
  void MaybeCompleteWatchTopology();
  void MaybeCompleteWatchElementState(fuchsia_hardware_audio_signalprocessing::ElementId);

  void Serve(fidl::ServerEnd<fuchsia_hardware_audio::Composite> server);
  void ResetRingBuffer();
  void OnRingBufferClosed(fidl::UnbindInfo info);
  void OnSignalProcessingClosed(fidl::UnbindInfo info);
  fuchsia_virtualaudio::RingBuffer& GetRingBuffer(uint64_t id);
  fuchsia_virtualaudio::Composite& composite_config() {
    return config_.device_specific()->composite().value();
  }

  std::optional<fidl::ServerBinding<fuchsia_hardware_audio::Composite>> composite_binding_;

  // This driver exposes only one ring buffer element.
  bool ring_buffer_is_outgoing_;
  fzl::VmoMapper ring_buffer_mapper_;
  uint32_t notifications_per_ring_ = 0;
  uint32_t num_ring_buffer_frames_ = 0;
  uint32_t frame_size_ = 4;
  zx::vmo ring_buffer_vmo_;

  // ring-buffer state
  bool ring_buffer_vmo_fetched_ = false;
  bool ring_buffer_started_ = false;
  std::optional<fuchsia_hardware_audio::Format> ring_buffer_format_;
  uint64_t ring_buffer_active_channel_mask_;
  zx::time active_channel_set_time_;

  // ring-buffer hanging gets
  bool should_reply_to_delay_request_ = true;
  std::optional<WatchDelayInfoCompleter::Async> delay_info_completer_;
  bool should_reply_to_position_request_ = true;
  std::optional<WatchClockRecoveryPositionInfoCompleter::Async> position_info_completer_;

  // This driver exposes only one DAI element.
  std::optional<fuchsia_hardware_audio::DaiFormat> dai_format_;

  fuchsia_virtualaudio::Configuration config_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio::RingBuffer>> ring_buffer_;
  std::optional<fidl::ServerBinding<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      signal_;

  std::vector<fuchsia_hardware_audio_signalprocessing::Element> elements_;
  std::unordered_map<fuchsia_hardware_audio_signalprocessing::ElementId,
                     fuchsia_hardware_audio_signalprocessing::Element*>
      element_map_;

  std::vector<fuchsia_hardware_audio_signalprocessing::Topology> topologies_;
  fuchsia_hardware_audio_signalprocessing::TopologyId current_topology_id_;
  std::optional<fuchsia_hardware_audio_signalprocessing::TopologyId> last_reported_topology_id_;
  std::optional<WatchTopologyCompleter::Async> watch_topology_completer_;

  struct ElementSnapshot {
    ElementSnapshot() = default;
    ElementSnapshot(const ElementSnapshot&) = delete;
    ElementSnapshot(ElementSnapshot&&) = default;
    ElementSnapshot& operator=(const ElementSnapshot&) = delete;
    ElementSnapshot& operator=(ElementSnapshot&&) = default;

    fuchsia_hardware_audio_signalprocessing::ElementState current;
    std::optional<fuchsia_hardware_audio_signalprocessing::ElementState> last_notified;
    std::optional<WatchElementStateCompleter::Async> completer;
  };
  std::unordered_map<fuchsia_hardware_audio_signalprocessing::ElementId, ElementSnapshot>
      element_states_;

  async_dispatcher_t* dispatcher_;
  fidl::ServerBinding<fuchsia_virtualaudio::Device> device_binding_;
  driver_devfs::Connector<fuchsia_hardware_audio::Composite> devfs_connector_{
      fit::bind_member<&VirtualAudioComposite::Serve>(this)};
  std::optional<fdf::OwnedChildNode> child_;
  InstanceId instance_id_;
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_VIRTUAL_AUDIO_COMPOSITE_H_

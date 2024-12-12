// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_H_

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/test_base.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/test_base.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/zx/channel.h>
#include <zircon/errors.h>

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <optional>
#include <string_view>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/testing/fake_composite_ring_buffer.h"

namespace media_audio {

static constexpr bool kLogFakeComposite = false;

class FakeCompositeRingBuffer;

// This driver implements the audio driver interface and is configurable to simulate audio hardware.
class FakeComposite final
    : public std::enable_shared_from_this<FakeComposite>,
      public fidl::testing::TestBase<fuchsia_hardware_audio::Composite>,
      public fidl::testing::TestBase<fuchsia_hardware_audio_signalprocessing::SignalProcessing> {
 public:
  static constexpr char kDefaultManufacturer[] = "fake_composite device manufacturer";
  static constexpr char kDefaultProduct[] = "fake_composite device product";
  static constexpr UniqueId kDefaultUniqueInstanceId{
      0xF1, 0xD3, 0xB5, 0x97, 0x79, 0x5B, 0x3D, 0x1F,
      0x0E, 0x2C, 0x4A, 0x68, 0x86, 0xA4, 0xC2, 0xE0,
  };
  static constexpr ClockDomain kDefaultClockDomain = fuchsia_hardware_audio::kClockDomainMonotonic;

  // DaiFormats and format sets
  //
  static constexpr uint32_t kDefaultDaiNumberOfChannels = 1;
  static constexpr uint32_t kDefaultDaiNumberOfChannels2 = 2;
  static constexpr fuchsia_hardware_audio::DaiSampleFormat kDefaultDaiSampleFormat =
      fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned;
  static constexpr fuchsia_hardware_audio::DaiSampleFormat kDefaultDaiSampleFormat2 =
      fuchsia_hardware_audio::DaiSampleFormat::kPcmFloat;
  static constexpr uint32_t kDefaultDaiFrameRate = 48000;
  static constexpr uint32_t kDefaultDaiFrameRate2 = 96000;
  static constexpr uint8_t kDefaultDaiBitsPerSlot = 16;
  static constexpr uint8_t kDefaultDaiBitsPerSlot2 = 32;
  static constexpr uint8_t kDefaultDaiBitsPerSample = 16;
  static constexpr uint8_t kDefaultDaiBitsPerSample2 = 32;

  static const fuchsia_hardware_audio::DaiFrameFormat kDefaultDaiFrameFormat;
  static const fuchsia_hardware_audio::DaiFrameFormat kDefaultDaiFrameFormat2;
  static const std::vector<uint32_t> kDefaultDaiNumberOfChannelsSet;
  static const std::vector<uint32_t> kDefaultDaiNumberOfChannelsSet2;
  static const std::vector<fuchsia_hardware_audio::DaiSampleFormat> kDefaultDaiSampleFormatsSet;
  static const std::vector<fuchsia_hardware_audio::DaiSampleFormat> kDefaultDaiSampleFormatsSet2;
  static const std::vector<fuchsia_hardware_audio::DaiFrameFormat> kDefaultDaiFrameFormatsSet;
  static const std::vector<fuchsia_hardware_audio::DaiFrameFormat> kDefaultDaiFrameFormatsSet2;
  static const std::vector<uint32_t> kDefaultDaiFrameRates;
  static const std::vector<uint32_t> kDefaultDaiFrameRates2;
  static const std::vector<uint8_t> kDefaultDaiBitsPerSlotSet;
  static const std::vector<uint8_t> kDefaultDaiBitsPerSlotSet2;
  static const std::vector<uint8_t> kDefaultDaiBitsPerSampleSet;
  static const std::vector<uint8_t> kDefaultDaiBitsPerSampleSet2;
  static const fuchsia_hardware_audio::DaiSupportedFormats kDefaultDaiFormatSet;
  static const fuchsia_hardware_audio::DaiSupportedFormats kDefaultDaiFormatSet2;
  static const std::vector<fuchsia_hardware_audio::DaiSupportedFormats> kDefaultDaiFormatSets;
  static const std::vector<fuchsia_hardware_audio::DaiSupportedFormats> kDefaultDaiFormatSets2;
  static const std::unordered_map<ElementId,
                                  std::vector<fuchsia_hardware_audio::DaiSupportedFormats>>
      kDefaultDaiFormatsMap;

  static const fuchsia_hardware_audio::DaiFormat kDefaultDaiFormat;
  static const fuchsia_hardware_audio::DaiFormat kDefaultDaiFormat2;

  // RingBufferFormats and format sets
  //
  static constexpr size_t kDefaultRingBufferAllocationSize = 8000;

  static constexpr uint8_t kDefaultRbNumberOfChannels = 2;
  static constexpr uint8_t kDefaultRbNumberOfChannels2 = 1;
  static constexpr uint32_t kDefaultRbChannelAttributeMinFrequency = 50;
  static constexpr uint32_t kDefaultRbChannelAttributeMinFrequency2 = 20000;
  static constexpr uint32_t kDefaultRbChannelAttributeMaxFrequency = 22000;
  static constexpr uint32_t kDefaultRbChannelAttributeMaxFrequency2 = 16000;
  static const fuchsia_hardware_audio::ChannelAttributes kDefaultRbAttributes;
  static const fuchsia_hardware_audio::ChannelAttributes kDefaultRbAttributes2;
  static const fuchsia_hardware_audio::ChannelAttributes kDefaultRbAttributes3;
  static const std::vector<fuchsia_hardware_audio::ChannelAttributes> kDefaultRbAttributeSet;
  static const std::vector<fuchsia_hardware_audio::ChannelAttributes> kDefaultRbAttributeSet2;
  static const fuchsia_hardware_audio::ChannelSet kDefaultRbChannelsSet;
  static const fuchsia_hardware_audio::ChannelSet kDefaultRbChannelsSet2;
  static const std::vector<fuchsia_hardware_audio::ChannelSet> kDefaultRbChannelsSets;
  static const std::vector<fuchsia_hardware_audio::ChannelSet> kDefaultRbChannelsSets2;

  static constexpr fuchsia_hardware_audio::SampleFormat kDefaultRbSampleFormat =
      fuchsia_hardware_audio::SampleFormat::kPcmSigned;
  static constexpr fuchsia_hardware_audio::SampleFormat kDefaultRbSampleFormat2 =
      fuchsia_hardware_audio::SampleFormat::kPcmSigned;
  static const std::vector<fuchsia_hardware_audio::SampleFormat> kDefaultRbSampleFormats;
  static const std::vector<fuchsia_hardware_audio::SampleFormat> kDefaultRbSampleFormats2;

  static constexpr uint8_t kDefaultRbBytesPerSample = 2;
  static constexpr uint8_t kDefaultRbBytesPerSample2 = 4;
  static const std::vector<uint8_t> kDefaultRbBytesPerSampleSet;
  static const std::vector<uint8_t> kDefaultRbBytesPerSampleSet2;

  static constexpr uint8_t kDefaultRbValidBitsPerSample = 16;
  static constexpr uint8_t kDefaultRbValidBitsPerSample2 = 20;
  static const std::vector<uint8_t> kDefaultRbValidBitsPerSampleSet;
  static const std::vector<uint8_t> kDefaultRbValidBitsPerSampleSet2;

  static constexpr uint32_t kDefaultRbFrameRate = 48000;
  static constexpr uint32_t kDefaultRbFrameRate2 = 44100;
  static const std::vector<uint32_t> kDefaultRbFrameRates;
  static const std::vector<uint32_t> kDefaultRbFrameRates2;

  static const fuchsia_hardware_audio::PcmSupportedFormats kDefaultPcmRingBufferFormatSet;
  static const fuchsia_hardware_audio::PcmSupportedFormats kDefaultPcmRingBufferFormatSet2;

  static const fuchsia_hardware_audio::SupportedFormats kDefaultRbFormatSet;
  static const fuchsia_hardware_audio::SupportedFormats kDefaultRbFormatSet2;

  static const std::vector<fuchsia_hardware_audio::SupportedFormats> kDefaultRbFormatSets;
  static const std::vector<fuchsia_hardware_audio::SupportedFormats> kDefaultRbFormatSets2;

  static const std::unordered_map<ElementId, std::vector<fuchsia_hardware_audio::SupportedFormats>>
      kDefaultRbFormatsMap;

  static const fuchsia_hardware_audio::PcmFormat kDefaultRbFormat;
  static const fuchsia_hardware_audio::PcmFormat kDefaultRbFormat2;

  // signalprocessing elements and topologies
  //
  // For min/max checks based on ranges, keep the DAI and RB element ID ranges contiguous.
  static constexpr ElementId kSourceDaiElementId = 0;
  static constexpr ElementId kDestDaiElementId = 1;
  static constexpr ElementId kMinDaiElementId = kSourceDaiElementId;
  static constexpr ElementId kMaxDaiElementId = kDestDaiElementId;

  static constexpr ElementId kDestRbElementId = 2;
  static constexpr ElementId kSourceRbElementId = 3;
  static constexpr ElementId kMinRingBufferElementId = kDestRbElementId;
  static constexpr ElementId kMaxRingBufferElementId = kSourceRbElementId;

  static constexpr ElementId kMuteElementId = 4;

  static constexpr ElementId kMinElementId = kSourceDaiElementId;
  static constexpr ElementId kMaxElementId = kMuteElementId;

  static const std::string kSourceDaiElementDescription;
  static const std::string kDestDaiElementDescription;
  static const std::string kSourceRbElementDescription;
  static const std::string kDestRbElementDescription;
  static const std::string kMuteElementDescription;

  static const fuchsia_hardware_audio_signalprocessing::Element kSourceDaiElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kDestRbElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kSourceRbElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kDestDaiElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kMuteElement;
  static const zx::duration kSourceDaiElementProcessingDelay;
  static const zx::duration kSourceRbElementProcessingDelay;
  static const zx::duration kDestDaiElementProcessingDelay;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kSourceDaiElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kDestRbElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kSourceRbElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kDestDaiElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kMuteElementInitState;
  static const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElements;

  // For min/max checks based on ranges, keep this range contiguous.
  static constexpr TopologyId kInputOnlyTopologyId = 10;
  static constexpr TopologyId kFullDuplexTopologyId = 11;
  static constexpr TopologyId kOutputOnlyTopologyId = 12;
  static constexpr TopologyId kOutputWithMuteTopologyId = 13;
  static constexpr TopologyId kMinTopologyId = kInputOnlyTopologyId;
  static constexpr TopologyId kMaxTopologyId = kOutputWithMuteTopologyId;

  static const fuchsia_hardware_audio_signalprocessing::EdgePair kTopologyInputEdgePair;
  static const fuchsia_hardware_audio_signalprocessing::EdgePair kTopologyOutputEdgePair;
  static const fuchsia_hardware_audio_signalprocessing::EdgePair kTopologyRbToMuteEdgePair;
  static const fuchsia_hardware_audio_signalprocessing::EdgePair kTopologyMuteToDaiEdgePair;
  static const fuchsia_hardware_audio_signalprocessing::Topology kInputOnlyTopology;
  static const fuchsia_hardware_audio_signalprocessing::Topology kFullDuplexTopology;
  static const fuchsia_hardware_audio_signalprocessing::Topology kOutputOnlyTopology;
  static const fuchsia_hardware_audio_signalprocessing::Topology kOutputWithMuteTopology;
  static const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologies;

  FakeComposite(zx::channel server_end, zx::channel client_end, async_dispatcher_t* dispatcher);

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ADR_WARN_OBJECT() << name;
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  // This returns a fidl::client_end<fuchsia_hardware_audio::Composite>. The driver will not start
  // serving requests until Enable is called, which is why we separate construction and Enable().
  fidl::ClientEnd<fuchsia_hardware_audio::Composite> Enable();
  void DropComposite();
  void DropChildren();
  void DropRingBuffers();
  void DropRingBuffer(ElementId element_id);
  static void on_rb_unbind(FakeCompositeRingBuffer* fake_ring_buffer, fidl::UnbindInfo info,
                           fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer>);
  void RingBufferWasDropped(ElementId element_id);

  // These may be called before the RingBuffer object is created; info must be cached until then.
  void ReserveRingBufferSize(ElementId element_id, size_t size);
  void EnableActiveChannelsSupport(ElementId element_id);
  void DisableActiveChannelsSupport(ElementId element_id);
  void PresetTurnOnDelay(ElementId element_id, std::optional<zx::duration> turn_on_delay);
  void PresetInternalExternalDelays(ElementId element_id, zx::duration internal_delay,
                                    std::optional<zx::duration> external_delay);

  async_dispatcher_t* dispatcher() { return dispatcher_; }
  bool is_bound() const { return binding_.has_value(); }

  bool responsive() const { return responsive_; }
  void set_responsive(bool responsive) { responsive_ = responsive; }
  std::optional<bool> health_state() const { return healthy_; }
  void set_health_state(std::optional<bool> healthy) { healthy_ = healthy; }

  void set_device_manufacturer(std::optional<std::string> mfgr) { manufacturer_ = std::move(mfgr); }
  void set_device_product(std::optional<std::string> product) { product_ = std::move(product); }
  void set_stream_unique_id(std::optional<UniqueId> uid) {
    if (uid) {
      std::memcpy(uid_->data(), uid->data(), sizeof(*uid));
    } else {
      uid_.reset();
    }
  }
  void set_clock_domain(std::optional<ClockDomain> clock_domain) { clock_domain_ = clock_domain; }

  bool is_ring_buffer(ElementId element_id) const {
    for (auto& element_iter : elements_) {
      if (element_iter.first == element_id) {
        return (element_iter.second.element.type() ==
                fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer);
      }
    }
    return false;  // We didn't find the element.
  }

  // These rely on the RingBuffer being created; do not use them to pre-configure the RingBuffer.
  uint64_t active_channels_bitmask(ElementId element_id) const {
    FX_CHECK(is_ring_buffer(element_id));
    return ring_buffers_.find(element_id)->second->active_channels_bitmask();
  }
  zx::time set_active_channels_completed_at(ElementId element_id) const {
    FX_CHECK(is_ring_buffer(element_id));
    return ring_buffers_.find(element_id)->second->set_active_channels_completed_at();
  }
  bool started(ElementId element_id) const {
    FX_CHECK(is_ring_buffer(element_id));
    return ring_buffers_.find(element_id)->second->started();
  }
  zx::time mono_start_time(ElementId element_id) const {
    FX_CHECK(is_ring_buffer(element_id));
    return ring_buffers_.find(element_id)->second->mono_start_time();
  }
  // Explicitly trigger a change notification, for the current values of gain/plug/delay.
  void InjectDelayUpdate(ElementId element_id, std::optional<zx::duration> internal_delay,
                         std::optional<zx::duration> external_delay) {
    FX_CHECK(is_ring_buffer(element_id));
    ring_buffers_.find(element_id)->second->InjectDelayUpdate(internal_delay, external_delay);
  }
  void InjectTopologyChange(std::optional<TopologyId> topology_id);
  void InjectElementStateChange(ElementId element_id,
                                fuchsia_hardware_audio_signalprocessing::ElementState new_state);

 private:
  friend FakeCompositeRingBuffer;

  static inline const std::string_view kClassName = "FakeComposite";

  struct FakeElementRecord {
    fuchsia_hardware_audio_signalprocessing::Element element;
    fuchsia_hardware_audio_signalprocessing::ElementState state;
    bool state_has_changed = true;  // immediately complete the first WatchElementState request
    std::optional<WatchElementStateCompleter::Async> watch_completer;
  };
  void SetupElementsMap();

  // fuchsia_hardware_audio::Composite implementation
  void Reset(ResetCompleter::Sync& completer) override;
  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                            GetRingBufferFormatsCompleter::Sync& completer) override;
  void CreateRingBuffer(CreateRingBufferRequest& request,
                        CreateRingBufferCompleter::Sync& completer) override;
  void GetDaiFormats(GetDaiFormatsRequest& request,
                     GetDaiFormatsCompleter::Sync& completer) override;
  void SetDaiFormat(SetDaiFormatRequest& request, SetDaiFormatCompleter::Sync& completer) override;

  // fuchsia_hardware_audio.Health implementation
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override;

  // fuchsia_hardware_audio_signalprocessing.Connector implementation
  void SignalProcessingConnect(SignalProcessingConnectRequest& request,
                               SignalProcessingConnectCompleter::Sync& completer) override;

  // fuchsia_hardware_audio_signalprocessing::SignalProcessing implementation (including Reader)
  void GetElements(GetElementsCompleter::Sync& completer) final;
  void GetTopologies(GetTopologiesCompleter::Sync& completer) final;
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) final;
  void WatchTopology(WatchTopologyCompleter::Sync& completer) final;
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) final;
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) final;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_audio_signalprocessing::SignalProcessing>
          metadata,
      fidl::UnknownMethodCompleter::Sync& completer) final;

  // Internal implementation methods/members
  static bool DaiFormatIsSupported(ElementId element_id,
                                   const fuchsia_hardware_audio::DaiFormat& format);

  static void MaybeCompleteWatchElementState(FakeElementRecord& element_record);
  void MaybeCompleteWatchTopology();

  async_dispatcher_t* dispatcher_;
  fidl::ServerEnd<fuchsia_hardware_audio::Composite> server_end_;
  fidl::ClientEnd<fuchsia_hardware_audio::Composite> client_end_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_audio::Composite>> binding_;

  bool responsive_ = true;
  std::optional<bool> healthy_ = true;
  std::optional<GetHealthStateCompleter::Async> health_completer_;

  std::optional<std::string> manufacturer_ = kDefaultManufacturer;
  std::optional<std::string> product_ = kDefaultProduct;
  std::optional<UniqueId> uid_ = kDefaultUniqueInstanceId;
  std::optional<ClockDomain> clock_domain_ = kDefaultClockDomain;

  bool supports_signalprocessing_ = true;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_audio_signalprocessing::SignalProcessing>>
      signal_processing_binding_;

  std::unordered_map<ElementId, FakeElementRecord> elements_;

  std::optional<WatchTopologyCompleter::Async> watch_topology_completer_;
  std::optional<TopologyId> topology_id_ = kFullDuplexTopologyId;
  bool topology_has_changed_ = true;

  std::unordered_map<ElementId, size_t> ring_buffer_allocation_sizes_;
  std::unordered_map<ElementId, bool> active_channels_support_overrides_;
  std::unordered_map<ElementId, std::optional<zx::duration>> turn_on_delay_overrides_;
  std::unordered_map<ElementId, zx::duration> internal_delay_overrides_;
  std::unordered_map<ElementId, std::optional<zx::duration>> external_delay_overrides_;

  std::unordered_map<ElementId, fidl::ServerBindingRef<fuchsia_hardware_audio::RingBuffer>>
      ring_buffer_bindings_;
  std::unordered_map<ElementId, std::unique_ptr<FakeCompositeRingBuffer>> ring_buffers_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_H_

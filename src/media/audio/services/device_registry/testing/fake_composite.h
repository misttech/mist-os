// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_H_

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/test_base.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/test_base.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/fit/result.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/channel.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>
#include <zircon/time.h>

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <optional>
#include <string_view>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

class FakeCompositeRingBuffer : public fidl::testing::TestBase<fuchsia_hardware_audio::RingBuffer> {
  static constexpr bool kLogFakeCompositeRingBuffer = false;
  static inline const std::string_view kClassName = "FakeCompositeRingBuffer";

  static constexpr bool kDefaultNeedsCacheFlushInvalidate = false;
  static constexpr std::optional<int64_t> kDefaultTurnOnDelay = std::nullopt;
  static constexpr uint32_t kDefaultDriverTransferBytes = 32;
  static constexpr bool kDefaultSupportsActiveChannels = false;
  static constexpr std::optional<zx_duration_t> kDefaultInternalDelay = ZX_USEC(20);

 public:
  FakeCompositeRingBuffer() = default;
  FakeCompositeRingBuffer(ElementId element_id, async_dispatcher_t* dispatcher,
                          fidl::ServerEnd<fuchsia_hardware_audio::RingBuffer> server_end,
                          fuchsia_hardware_audio::PcmFormat format,
                          size_t ring_buffer_allocated_size)
      : TestBase(),
        element_id_(element_id),
        dispatcher_(dispatcher),
        format_(std::move(format)),
        bytes_per_frame_(format_.number_of_channels() * format_.bytes_per_sample()),
        active_channels_bitmask_((1u << format_.number_of_channels()) - 1u),
        active_channels_set_time_(zx::clock::get_monotonic()) {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    AllocateRingBuffer(element_id_, ring_buffer_allocated_size);
    binding_ = fidl::BindServer(dispatcher_, std::move(server_end), this);
  }

  void AllocateRingBuffer(ElementId element_id, size_t size) {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    FX_CHECK(!vmo_.is_valid()) << "Calling AllocateRingBuffer multiple times is not supported";
    allocated_size_ = size;

    fzl::VmoMapper mapper;
    mapper.CreateAndMap(allocated_size_, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &vmo_);
  }

  void Drop() {
    binding_->Close(ZX_ERR_PEER_CLOSED);
    binding_.reset();
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    FX_LOGS(WARNING) << "FakeCompositeRingBuffer(" << this << ")::NotImplemented: " << name;
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  void GetProperties(GetPropertiesCompleter::Sync& completer) override {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    fuchsia_hardware_audio::RingBufferProperties props;
    if (needs_cache_flush_or_invalidate_) {
      props.needs_cache_flush_or_invalidate(*needs_cache_flush_or_invalidate_);
    }
    if (turn_on_delay_) {
      props.turn_on_delay(*turn_on_delay_);
    }
    if (driver_transfer_bytes_) {
      props.driver_transfer_bytes(*driver_transfer_bytes_);
    }
    completer.Reply(props);
  }

  void GetVmo(GetVmoRequest& request, GetVmoCompleter::Sync& completer) override {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    auto total_requested_size =
        driver_transfer_bytes_.value_or(0) + request.min_frames() * bytes_per_frame_;
    if (total_requested_size > allocated_size_) {
      ADR_WARN_METHOD() << "Requested size " << total_requested_size << " exceeds allocated size "
                        << allocated_size_;
      completer.Reply(fit::error(fuchsia_hardware_audio::GetVmoError::kInvalidArgs));
      return;
    }
    clock_recovery_notifications_per_ring_ = request.clock_recovery_notifications_per_ring();
    requested_frames_ = (total_requested_size - 1) / bytes_per_frame_ + 1;

    // Dup our ring buffer VMO to send over the channel.
    zx::vmo out_vmo;
    FX_CHECK(vmo_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_vmo) == ZX_OK);

    completer.Reply(zx::ok(fuchsia_hardware_audio::RingBufferGetVmoResponse{{
        .num_frames = requested_frames_,
        .ring_buffer = std::move(out_vmo),
    }}));
  }

  void Start(StartCompleter::Sync& completer) override {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    if (!vmo_.is_valid() || started_) {
      completer.Close(ZX_ERR_BAD_STATE);
      return;
    }
    started_ = true;
    start_time_ = zx::clock::get_monotonic();
    completer.Reply(start_time_.get());
  }

  void Stop(StopCompleter::Sync& completer) override {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    if (!vmo_.is_valid() || !started_) {
      completer.Close(ZX_ERR_BAD_STATE);
      return;
    }
    started_ = false;
    start_time_ = zx::time(0);
    completer.Reply();
  }

  void SetActiveChannels(SetActiveChannelsRequest& request,
                         SetActiveChannelsCompleter::Sync& completer) override {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    if (!supports_active_channels_) {
      completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
      return;
    }
    if (request.active_channels_bitmask() >= (1u << format_.number_of_channels())) {
      completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
      return;
    }
    if (active_channels_bitmask_ != request.active_channels_bitmask()) {
      active_channels_bitmask_ = request.active_channels_bitmask();
      active_channels_set_time_ = zx::clock::get_monotonic();
    }
    completer.Reply(zx::ok(active_channels_set_time_.get()));
  }

  void WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) override {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    if (watch_delay_info_completer_.has_value()) {
      completer.Close(ZX_ERR_BAD_STATE);
      return;
    }

    watch_delay_info_completer_ = completer.ToAsync();
    MaybeCompleteWatchDelayInfo();
  }

  void InjectDelayChange(std::optional<zx_duration_t> internal_delay,
                         std::optional<zx_duration_t> external_delay) {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    if (internal_delay.value_or(0) != internal_delay_.value_or(0) ||
        external_delay.value_or(0) != external_delay_.value_or(0)) {
      delays_have_changed_ = true;
    }
    internal_delay_ = internal_delay;
    external_delay_ = external_delay;

    MaybeCompleteWatchDelayInfo();
  }

  void MaybeCompleteWatchDelayInfo() {
    ADR_LOG_METHOD(kLogFakeCompositeRingBuffer);
    if (delays_have_changed_ && watch_delay_info_completer_) {
      delays_have_changed_ = false;

      auto completer = std::move(*watch_delay_info_completer_);
      watch_delay_info_completer_.reset();

      fuchsia_hardware_audio::DelayInfo info;
      if (internal_delay_) {
        info.internal_delay(*internal_delay_);
      }
      if (external_delay_) {
        info.external_delay(*external_delay_);
      }
      completer.Reply(std::move(info));
    }
  }

  void WatchClockRecoveryPositionInfo(
      WatchClockRecoveryPositionInfoCompleter::Sync& completer) override {
    NotImplemented_("WatchClockRecoveryPositionInfo", completer);
  }

  // Accessors
  ElementId element_id() const { return element_id_; }

  bool started() const { return started_; }
  zx::time start_time() const { return start_time_; }

  void enable_active_channels_support() { supports_active_channels_ = true; }
  void disable_active_channels_support() { supports_active_channels_ = false; }
  uint64_t active_channels_bitmask() const { return active_channels_bitmask_; }
  zx::time active_channels_set_time() const { return active_channels_set_time_; }

 private:
  // ctor
  ElementId element_id_;
  async_dispatcher_t* dispatcher_;
  fuchsia_hardware_audio::PcmFormat format_;
  uint32_t bytes_per_frame_;

  // Bind
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_audio::RingBuffer>> binding_;

  // GetProperties
  std::optional<bool> needs_cache_flush_or_invalidate_ = kDefaultNeedsCacheFlushInvalidate;
  std::optional<int64_t> turn_on_delay_ = kDefaultTurnOnDelay;
  std::optional<uint32_t> driver_transfer_bytes_ = kDefaultDriverTransferBytes;

  // GetVmo
  uint32_t requested_frames_;
  zx::vmo vmo_;
  size_t allocated_size_;

  // Start / Stop
  bool started_ = false;
  zx::time start_time_;

  // SetActiveChannels
  bool supports_active_channels_ = kDefaultSupportsActiveChannels;
  uint64_t active_channels_bitmask_;
  zx::time active_channels_set_time_;

  // WatchDelayInfo
  std::optional<WatchDelayInfoCompleter::Async> watch_delay_info_completer_;
  std::optional<zx_duration_t> internal_delay_ = kDefaultInternalDelay;
  std::optional<zx_duration_t> external_delay_;
  bool delays_have_changed_ = true;

  // WatchClockRecoveryPositionInfo
  uint32_t clock_recovery_notifications_per_ring_ = 0;
};

// This driver implements the audio driver interface and is configurable to simulate audio hardware.
using fuchsia_hardware_audio::Composite;
using fuchsia_hardware_audio::CompositeConnector;
using fuchsia_hardware_audio_signalprocessing::SignalProcessing;
class FakeComposite : public fidl::testing::TestBase<Composite>,
                      public fidl::testing::TestBase<SignalProcessing> {
  static constexpr bool kLogFakeComposite = false;

 public:
  static constexpr char kDefaultManufacturer[] = "fake_composite device manufacturer";
  static constexpr char kDefaultProduct[] = "fake_composite device product";
  static constexpr UniqueId kDefaultUniqueInstanceId{
      0xF1, 0xD3, 0xB5, 0x97, 0x79, 0x5B, 0x3D, 0x1F,
      0x0E, 0x2C, 0x4A, 0x68, 0x86, 0xA4, 0xC2, 0xE0,
  };

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

  static constexpr size_t kDefaultRingBufferAllocationSize = 8000;
  // RingBufferFormats and format sets
  //
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

  static constexpr ElementId kMinElementId = kSourceDaiElementId;
  static constexpr ElementId kMaxElementId = kSourceRbElementId;

  static const fuchsia_hardware_audio_signalprocessing::Element kSourceDaiElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kDestRbElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kSourceRbElement;
  static const fuchsia_hardware_audio_signalprocessing::Element kDestDaiElement;
  static const fuchsia_hardware_audio_signalprocessing::Latency kSourceDaiElementLatency;
  static const fuchsia_hardware_audio_signalprocessing::Latency kDestRbElementLatency;
  static const fuchsia_hardware_audio_signalprocessing::Latency kSourceRbElementLatency;
  static const fuchsia_hardware_audio_signalprocessing::Latency kDestDaiElementLatency;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kSourceDaiElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kDestRbElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kSourceRbElementInitState;
  static const fuchsia_hardware_audio_signalprocessing::ElementState kDestDaiElementInitState;
  static const std::vector<fuchsia_hardware_audio_signalprocessing::Element> kElements;

  // For min/max checks based on ranges, keep this range contiguous.
  static constexpr TopologyId kInputOnlyTopologyId = 10;
  static constexpr TopologyId kFullDuplexTopologyId = 11;
  static constexpr TopologyId kOutputOnlyTopologyId = 12;
  static constexpr TopologyId kMinTopologyId = kInputOnlyTopologyId;
  static constexpr TopologyId kMaxTopologyId = kOutputOnlyTopologyId;

  static const fuchsia_hardware_audio_signalprocessing::EdgePair kTopologyInputEdgePair;
  static const fuchsia_hardware_audio_signalprocessing::EdgePair kTopologyOutputEdgePair;
  static const fuchsia_hardware_audio_signalprocessing::Topology kInputOnlyTopology;
  static const fuchsia_hardware_audio_signalprocessing::Topology kFullDuplexTopology;
  static const fuchsia_hardware_audio_signalprocessing::Topology kOutputOnlyTopology;
  static const std::vector<fuchsia_hardware_audio_signalprocessing::Topology> kTopologies;

  FakeComposite(zx::channel server_end, zx::channel client_end, async_dispatcher_t* dispatcher);
  ~FakeComposite() override;

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ADR_WARN_OBJECT() << name;
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  // This returns a fidl::client_end<Composite>. The driver will not start serving requests until
  // Enable is called, which is why the construction/Enable separation exists.
  fidl::ClientEnd<fuchsia_hardware_audio::Composite> Enable();
  void DropComposite();

  void ReserveRingBufferSize(ElementId element_id, size_t size);
  void EnableActiveChannelsSupport(ElementId element_id);
  void DisableActiveChannelsSupport(ElementId element_id);

  void DropRingBuffer(ElementId element_id) {
    if (auto match = ring_buffers_.find(element_id); match != ring_buffers_.end()) {
      match->second->Drop();
    }
  }

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

  void InjectElementStateChange(ElementId element_id,
                                fuchsia_hardware_audio_signalprocessing::ElementState new_state);
  void InjectTopologyChange(std::optional<TopologyId> topology_id);

  ////////////////////////////////////////////////////////////////
 private:
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

  // fuchsia.hardware.audio.Health implementation
  void GetHealthState(GetHealthStateCompleter::Sync& completer) override;

  // fuchsia.hardware.audio.signalprocessing.Connector implementation
  void SignalProcessingConnect(SignalProcessingConnectRequest& request,
                               SignalProcessingConnectCompleter::Sync& completer) override;

  // fuchsia.hardware.audio.signalprocessing.SignalProcessing implementation (including Reader)
  void GetElements(GetElementsCompleter::Sync& completer) final;
  void GetTopologies(GetTopologiesCompleter::Sync& completer) final;
  void WatchElementState(WatchElementStateRequest& request,
                         WatchElementStateCompleter::Sync& completer) final;
  void WatchTopology(WatchTopologyCompleter::Sync& completer) final;
  void SetElementState(SetElementStateRequest& request,
                       SetElementStateCompleter::Sync& completer) final;
  void SetTopology(SetTopologyRequest& request, SetTopologyCompleter::Sync& completer) final;

  // Internal implementation methods/members
  static bool DaiFormatIsSupported(ElementId element_id,
                                   const fuchsia_hardware_audio::DaiFormat& format);

  static void CheckForElementStateCompletion(FakeElementRecord& element_record);
  void CheckForTopologyCompletion();

  async_dispatcher_t* dispatcher_;
  fidl::ServerEnd<fuchsia_hardware_audio::Composite> server_end_;
  fidl::ClientEnd<fuchsia_hardware_audio::Composite> client_end_;
  std::optional<fidl::ServerBindingRef<Composite>> binding_;

  bool responsive_ = true;
  std::optional<bool> healthy_ = true;
  std::optional<GetHealthStateCompleter::Async> health_completer_;

  std::optional<std::string> manufacturer_ = kDefaultManufacturer;
  std::optional<std::string> product_ = kDefaultProduct;
  std::optional<UniqueId> uid_ = kDefaultUniqueInstanceId;
  std::optional<ClockDomain> clock_domain_ = fuchsia_hardware_audio::kClockDomainMonotonic;

  bool supports_signalprocessing_ = true;
  std::optional<fidl::ServerBindingRef<SignalProcessing>> signal_processing_binding_;

  std::unordered_map<ElementId, FakeElementRecord> elements_;

  std::optional<WatchTopologyCompleter::Async> watch_topology_completer_;
  std::optional<TopologyId> topology_id_ = kFullDuplexTopologyId;
  bool topology_has_changed_ = true;

  std::unordered_map<ElementId, size_t> ring_buffer_allocation_sizes_;
  std::unordered_map<ElementId, bool> active_channels_support_overrides_;
  std::unordered_map<ElementId, std::unique_ptr<FakeCompositeRingBuffer>> ring_buffers_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_COMPOSITE_H_

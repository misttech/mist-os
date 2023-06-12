// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual_audio/virtual_audio_composite.h"

#include <lib/ddk/debug.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <audio-proto-utils/format-utils.h>
#include <fbl/algorithm.h>

namespace virtual_audio {

// static
int VirtualAudioComposite::instance_count_ = 0;

// static
fuchsia_virtualaudio::Configuration VirtualAudioComposite::GetDefaultConfig() {
  constexpr uint64_t kDefaultRingBufferId = 123;
  constexpr uint64_t kDefauttDaiId = 456;
  constexpr uint64_t kDefaultTopologyId = 789;

  fuchsia_virtualaudio::Configuration config;
  config.device_name("Virtual Audio Composite Device");
  config.manufacturer_name("Fuchsia Virtual Audio Group");
  config.product_name("Virgil v2, a Virtual Volume Vessel");
  config.unique_id(std::array<uint8_t, 16>({1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0}));

  fuchsia_virtualaudio::Composite composite = {};

  // Composite ring buffer.
  fuchsia_virtualaudio::CompositeRingBuffer composite_ring_buffer = {};

  // Ring Buffer.
  fuchsia_virtualaudio::RingBuffer ring_buffer = {};

  // By default we expose a single ring buffer format is 48kHz stereo 16bit.
  fuchsia_virtualaudio::FormatRange format = {};
  format.sample_format_flags(AUDIO_SAMPLE_FORMAT_16BIT);
  format.min_frame_rate(48'000);
  format.max_frame_rate(48'000);
  format.min_channels(2);
  format.max_channels(2);
  format.rate_family_flags(ASF_RANGE_FLAG_FPS_48000_FAMILY);
  ring_buffer.supported_formats(
      std::optional<std::vector<fuchsia_virtualaudio::FormatRange>>{std::in_place, {format}});

  // Default FIFO is 250 usec, at 48k stereo 16, no external delay specified.
  ring_buffer.driver_transfer_bytes(48);
  ring_buffer.internal_delay(0);

  // No ring_buffer_constraints specified.
  // No notifications_per_ring specified.

  composite_ring_buffer.id(kDefaultRingBufferId);
  composite_ring_buffer.ring_buffer(std::move(ring_buffer));

  std::vector<fuchsia_virtualaudio::CompositeRingBuffer> composite_ring_buffers = {};
  composite_ring_buffers.push_back(std::move(composite_ring_buffer));
  composite.ring_buffers(std::move(composite_ring_buffers));

  // Composite DAI interconnect.
  fuchsia_virtualaudio::CompositeDaiInterconnect composite_dai_interconnect = {};

  // DAI interconnect.
  fuchsia_virtualaudio::DaiInterconnect dai_interconnect = {};

  // By default we expose a single DAI format is 48kHz I2S (2 channels of 16-in-32, total frame size
  // 8 bytes).
  fuchsia_hardware_audio::DaiSupportedFormats item = {};
  item.number_of_channels(std::vector<uint32_t>{2});
  item.sample_formats(std::vector{fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned});
  item.frame_formats(std::vector{fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
      fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)});
  item.frame_rates(std::vector<uint32_t>{48'000});
  item.bits_per_slot(std::vector<uint8_t>{32});
  item.bits_per_sample(std::vector<uint8_t>{16});

  dai_interconnect.dai_supported_formats(
      std::optional<std::vector<fuchsia_hardware_audio::DaiSupportedFormats>>{std::in_place,
                                                                              {item}});

  composite_dai_interconnect.id(kDefauttDaiId);
  composite_dai_interconnect.dai_interconnect(std::move(dai_interconnect));
  std::vector<fuchsia_virtualaudio::CompositeDaiInterconnect> composite_dai_interconnects = {};
  composite_dai_interconnects.push_back(std::move(composite_dai_interconnect));
  composite.dai_interconnects(std::move(composite_dai_interconnects));

  // Topology with one ring buffer into one DAI interconnect.
  fuchsia_hardware_audio_signalprocessing::Topology topology;
  topology.id(kDefaultTopologyId);
  fuchsia_hardware_audio_signalprocessing::EdgePair edge;

  edge.processing_element_id_from(kDefaultRingBufferId).processing_element_id_to(kDefauttDaiId);
  topology.processing_elements_edge_pairs(std::vector({std::move(edge)}));
  composite.topologies(
      std::optional<std::vector<fuchsia_hardware_audio_signalprocessing::Topology>>{
          std::in_place, {std::move(topology)}});

  // Clock properties with no rate_adjustment_ppm specified (defaults to 0).
  fuchsia_virtualaudio::ClockProperties clock_properties = {};
  clock_properties.domain(0);
  composite.clock_properties(std::move(clock_properties));

  config.device_specific() =
      fuchsia_virtualaudio::DeviceSpecific::WithComposite(std::move(composite));

  return config;
}

VirtualAudioComposite::VirtualAudioComposite(fuchsia_virtualaudio::Configuration config,
                                             std::weak_ptr<VirtualAudioDeviceImpl> owner,
                                             zx_device_t* parent)
    : VirtualAudioCompositeDeviceType(parent),
      parent_(std::move(owner)),
      config_(std::move(config)) {
  ddk_proto_id_ = ZX_PROTOCOL_AUDIO_COMPOSITE;
  sprintf(instance_name_, "virtual-audio-composite-%d", instance_count_++);
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs(instance_name_));
  ZX_ASSERT_MSG(status == ZX_OK, "DdkAdd failed");
}

fuchsia_virtualaudio::RingBuffer& VirtualAudioComposite::GetRingBuffer(uint64_t id) {
  // TODO(fxbug.dev/124865): Add support for more than one and only one ring buffer.
  ZX_ASSERT(id == kRingBufferId);
  auto& ring_buffers = config_.device_specific()->composite()->ring_buffers().value();
  ZX_ASSERT(ring_buffers.size() == 1);
  ZX_ASSERT(ring_buffers[0].ring_buffer().has_value());
  return ring_buffers[0].ring_buffer().value();
}

fit::result<VirtualAudioComposite::ErrorT, CurrentFormat> VirtualAudioComposite::GetFormatForVA() {
  if (!ring_buffer_format_.has_value() || !ring_buffer_format_->pcm_format().has_value()) {
    zxlogf(WARNING, "ring buffer not initialized yet");
    return fit::error(ErrorT::kNoRingBuffer);
  }
  auto& pcm_format = ring_buffer_format_->pcm_format();

  auto& ring_buffer = GetRingBuffer(kRingBufferId);
  int64_t external_delay = 0;
  if (ring_buffer.external_delay().has_value()) {
    external_delay = ring_buffer.external_delay().value();
  };
  return fit::ok(CurrentFormat{
      .frames_per_second = pcm_format->frame_rate(),
      .sample_format = audio::utils::GetSampleFormat(pcm_format->valid_bits_per_sample(),
                                                     pcm_format->bytes_per_sample() * 8),
      .num_channels = pcm_format->number_of_channels(),
      .external_delay = zx::nsec(external_delay),
  });
}

fit::result<VirtualAudioComposite::ErrorT, CurrentBuffer> VirtualAudioComposite::GetBufferForVA() {
  if (!ring_buffer_vmo_.is_valid()) {
    zxlogf(WARNING, "ring buffer not initialized yet");
    return fit::error(ErrorT::kNoRingBuffer);
  }

  zx::vmo dup_vmo;
  zx_status_t status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &dup_vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "ring buffer creation failed: %s", zx_status_get_string(status));
    return fit::error(ErrorT::kInternal);
  }

  return fit::ok(CurrentBuffer{
      .vmo = std::move(dup_vmo),
      .num_frames = num_ring_buffer_frames_,
      .notifications_per_ring = notifications_per_ring_,
  });
}

void VirtualAudioComposite::Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) {
  fidl::BindServer(dispatcher(), std::move(request->composite_protocol), this,
                   [](virtual_audio::VirtualAudioComposite*, fidl::UnbindInfo info,
                      fidl::ServerEnd<fuchsia_hardware_audio::Composite>) {
                     // Do not log canceled cases which happens too often in particular in test
                     // cases.
                     if (info.status() != ZX_ERR_CANCELED) {
                       zxlogf(INFO, "Composite connect channel closing: %s",
                              info.FormatDescription().c_str());
                     }
                   });
}

void VirtualAudioComposite::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync& completer) {
  fidl::Arena arena;
  fuchsia_hardware_audio::CompositeProperties properties;
  properties.unique_id(config_.unique_id());
  properties.product(config_.product_name());
  properties.manufacturer(config_.manufacturer_name());
  ZX_ASSERT(composite_config().clock_properties().has_value());
  properties.clock_domain(composite_config().clock_properties()->domain());
  completer.Reply(std::move(properties));
}

void VirtualAudioComposite::GetDaiFormats(GetDaiFormatsRequest& request,
                                          GetDaiFormatsCompleter::Sync& completer) {
  // One DAI interconnect is supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more DAI interconnects, allowing their
  // configuration and observability via the virtual audio FIDL APIs.
  if (request.processing_element_id() != kDaiId) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  auto& dai_interconnects = composite_config().dai_interconnects().value();
  ZX_ASSERT(dai_interconnects.size() == 1);  // Supports only one and only one DAI interconnect.
  ZX_ASSERT(dai_interconnects[0].dai_interconnect().has_value());
  ZX_ASSERT(dai_interconnects[0].dai_interconnect()->dai_supported_formats().has_value());
  completer.Reply(zx::ok(dai_interconnects[0].dai_interconnect()->dai_supported_formats().value()));
}

void VirtualAudioComposite::SetDaiFormat(SetDaiFormatRequest& request,
                                         SetDaiFormatCompleter::Sync& completer) {
  // One DAI interconnect is supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more DAI interconnects, allowing their
  // configuration and observability via the virtual audio FIDL APIs.
  if (request.processing_element_id() != kDaiId) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
}

void VirtualAudioComposite::GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                                                 GetRingBufferFormatsCompleter::Sync& completer) {
  // One ring buffer is supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more ring buffers, allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  if (request.processing_element_id() != kRingBufferId) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  std::vector<fuchsia_hardware_audio::SupportedFormats> all_formats;
  auto& ring_buffer = GetRingBuffer(request.processing_element_id());
  for (auto& formats : ring_buffer.supported_formats().value()) {
    fuchsia_hardware_audio::PcmSupportedFormats pcm_formats;
    std::vector<fuchsia_hardware_audio::ChannelSet> channel_sets;
    for (uint8_t number_of_channels = formats.min_channels();
         number_of_channels <= formats.max_channels(); ++number_of_channels) {
      // Vector with number_of_channels empty attributes.
      std::vector<fuchsia_hardware_audio::ChannelAttributes> attributes(number_of_channels);
      fuchsia_hardware_audio::ChannelSet channel_set;
      channel_set.attributes(std::move(attributes));
      channel_sets.push_back(std::move(channel_set));
    }
    pcm_formats.channel_sets(std::move(channel_sets));

    std::vector<uint32_t> frame_rates;
    audio_stream_format_range_t range;
    range.sample_formats = formats.sample_format_flags();
    range.min_frames_per_second = formats.min_frame_rate();
    range.max_frames_per_second = formats.max_frame_rate();
    range.min_channels = formats.min_channels();
    range.max_channels = formats.max_channels();
    range.flags = formats.rate_family_flags();
    audio::utils::FrameRateEnumerator enumerator(range);
    for (uint32_t frame_rate : enumerator) {
      frame_rates.push_back(frame_rate);
    }
    pcm_formats.frame_rates(std::move(frame_rates));

    std::vector<audio::utils::Format> formats2 =
        audio::utils::GetAllFormats(formats.sample_format_flags());
    for (audio::utils::Format& format : formats2) {
      std::vector<fuchsia_hardware_audio::SampleFormat> sample_formats{format.format};
      std::vector<uint8_t> bytes_per_sample{format.bytes_per_sample};
      std::vector<uint8_t> valid_bits_per_sample{format.valid_bits_per_sample};
      auto pcm_formats2 = pcm_formats;
      pcm_formats2.sample_formats(std::move(sample_formats));
      pcm_formats2.bytes_per_sample(std::move(bytes_per_sample));
      pcm_formats2.valid_bits_per_sample(std::move(valid_bits_per_sample));
      fuchsia_hardware_audio::SupportedFormats formats_entry;
      formats_entry.pcm_supported_formats(std::move(pcm_formats2));
      all_formats.push_back(std::move(formats_entry));
    }
  }
  completer.Reply(zx::ok(std::move(all_formats)));
}

void VirtualAudioComposite::OnRingBufferClosed(fidl::UnbindInfo info) {
  // Do not log canceled cases which happens too often in particular in test cases.
  if (info.status() != ZX_ERR_CANCELED) {
    zxlogf(INFO, "Ring buffer channel closing: %s", info.FormatDescription().c_str());
  }
  ResetRingBuffer();
}

void VirtualAudioComposite::CreateRingBuffer(CreateRingBufferRequest& request,
                                             CreateRingBufferCompleter::Sync& completer) {
  // One ring buffer is supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more ring buffers, allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  if (request.processing_element_id() != kRingBufferId) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  ring_buffer_format_.emplace(std::move(request.format()));
  ring_buffer_.emplace(dispatcher(), std::move(request.ring_buffer()), this,
                       std::mem_fn(&VirtualAudioComposite::OnRingBufferClosed));
  completer.Reply(zx::ok());
}

void VirtualAudioComposite::ResetRingBuffer() {
  watch_delay_replied_ = false;
  watch_position_replied_ = false;
  ring_buffer_vmo_fetched_ = false;
  ring_buffer_started_ = false;
  notifications_per_ring_ = 0;
  position_info_completer_.reset();
  delay_info_completer_.reset();
  // We don't reset ring_buffer_format_ and dai_format_ to allow for retrieval for
  // observability.
}

void VirtualAudioComposite::GetProperties(
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync& completer) {
  fidl::Arena arena;
  fuchsia_hardware_audio::RingBufferProperties properties;
  auto& ring_buffer = GetRingBuffer(kRingBufferId);
  properties.needs_cache_flush_or_invalidate(false).driver_transfer_bytes(
      ring_buffer.driver_transfer_bytes());
  completer.Reply(std::move(properties));
}

void VirtualAudioComposite::GetVmo(GetVmoRequest& request, GetVmoCompleter::Sync& completer) {
  if (ring_buffer_mapper_.start() != nullptr) {
    ring_buffer_mapper_.Unmap();
  }

  uint32_t min_frames = 0;
  uint32_t modulo_frames = 1;
  auto& ring_buffer = GetRingBuffer(kRingBufferId);
  if (ring_buffer.ring_buffer_constraints().has_value()) {
    min_frames = ring_buffer.ring_buffer_constraints()->min_frames();
    modulo_frames = ring_buffer.ring_buffer_constraints()->modulo_frames();
  }
  num_ring_buffer_frames_ =
      std::max(min_frames, fbl::round_up<uint32_t, uint32_t>(request.min_frames(), modulo_frames));

  zx_status_t status = ring_buffer_mapper_.CreateAndMap(
      num_ring_buffer_frames_ * frame_size_, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
      &ring_buffer_vmo_,
      ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP | ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER);

  ZX_ASSERT_MSG(status == ZX_OK, "failed to create ring buffer VMO: %s",
                zx_status_get_string(status));

  zx::vmo out_vmo;
  status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &out_vmo);
  ZX_ASSERT_MSG(status == ZX_OK, "failed to duplicate VMO handle for out param: %s",
                zx_status_get_string(status));

  notifications_per_ring_ = request.clock_recovery_notifications_per_ring();

  zx::vmo duplicate_vmo;
  status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &duplicate_vmo);
  ZX_ASSERT_MSG(status == ZX_OK, "failed to duplicate VMO handle for VA client: %s",
                zx_status_get_string(status));

  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  parent->NotifyBufferCreated(std::move(duplicate_vmo), num_ring_buffer_frames_,
                              notifications_per_ring_);
  fuchsia_hardware_audio::RingBufferGetVmoResponse response;
  response.num_frames(num_ring_buffer_frames_);
  response.ring_buffer(std::move(out_vmo));
  completer.Reply(zx::ok(std::move(response)));
  ring_buffer_vmo_fetched_ = true;
}

void VirtualAudioComposite::Start(StartCompleter::Sync& completer) {
  if (!ring_buffer_vmo_fetched_) {
    zxlogf(ERROR, "Cannot start the ring buffer before retrieving the VMO");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (ring_buffer_started_) {
    zxlogf(ERROR, "Cannot start the ring buffer if already started");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  zx_time_t now = zx::clock::get_monotonic().get();
  parent->NotifyStart(now);
  completer.Reply(now);
  ring_buffer_started_ = true;
}

void VirtualAudioComposite::Stop(StopCompleter::Sync& completer) {
  if (!ring_buffer_vmo_fetched_) {
    zxlogf(ERROR, "Cannot start the ring buffer before retrieving the VMO");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (!ring_buffer_started_) {
    zxlogf(INFO, "Stop called while stopped; doing nothing");
    completer.Reply();
    return;
  }
  auto parent = parent_.lock();
  ZX_ASSERT(parent);
  zx_time_t now = zx::clock::get_monotonic().get();
  // TODO(fxbug.dev/124865): Add support for position, now we always report 0.
  parent->NotifyStop(now, 0);
  completer.Reply();
  ring_buffer_started_ = false;
}

void VirtualAudioComposite::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  if (!watch_position_replied_) {
    fuchsia_hardware_audio::RingBufferPositionInfo position_info;
    position_info.timestamp(zx::clock::get_monotonic().get());
    // TODO(fxbug.dev/124865): Add support for position, now we always report 0.
    position_info.position(0);
    completer.Reply(std::move(position_info));
    watch_position_replied_ = true;
  } else if (!position_info_completer_) {
    position_info_completer_.emplace(completer.ToAsync());
  } else {
    // The client called WatchClockRecoveryPositionInfo when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    zxlogf(ERROR,
           "WatchClockRecoveryPositionInfo called when another hanging get was pending, unbinding");
    completer.Close(ZX_ERR_BAD_STATE);
  }
}

void VirtualAudioComposite::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  if (!watch_delay_replied_) {
    fuchsia_hardware_audio::DelayInfo delay_info;
    // TODO(fxbug.dev/124865): Add support for position, now we always report 0.
    auto& ring_buffer = GetRingBuffer(kRingBufferId);
    delay_info.internal_delay(ring_buffer.internal_delay());
    delay_info.external_delay(ring_buffer.external_delay());
    completer.Reply(std::move(delay_info));
    watch_delay_replied_ = true;
  } else if (!delay_info_completer_) {
    delay_info_completer_.emplace(completer.ToAsync());
  } else {
    // The client called WatchDelayInfo when another hanging get was pending.
    // This is an error condition and hence we unbind the channel.
    zxlogf(ERROR, "WatchDelayInfo called when another hanging get was pending, unbinding");
    completer.Close(ZX_ERR_BAD_STATE);
  }
}

void VirtualAudioComposite::SetActiveChannels(
    fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
    SetActiveChannelsCompleter::Sync& completer) {
  // TODO(fxbug.dev/81649): Add support.
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

void VirtualAudioComposite::OnSignalProcessingClosed(fidl::UnbindInfo info) {
  if (info.is_peer_closed()) {
    zxlogf(INFO, "Client disconnected");
  } else if (!info.is_user_initiated()) {
    // Do not log canceled cases which happens too often in particular in test cases.
    if (info.status() != ZX_ERR_CANCELED) {
      zxlogf(ERROR, "Client connection unbound: %s", info.status_string());
    }
  }
  if (signal_) {
    signal_.reset();
  }
}

void VirtualAudioComposite::SignalProcessingConnect(
    SignalProcessingConnectRequest& request, SignalProcessingConnectCompleter::Sync& completer) {
  if (signal_) {
    zxlogf(ERROR, "Signal processing already bound");
    request.protocol().Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  signal_.emplace(dispatcher(), std::move(request.protocol()), this,
                  std::mem_fn(&VirtualAudioComposite::OnSignalProcessingClosed));
}

void VirtualAudioComposite::GetElements(GetElementsCompleter::Sync& completer) {
  // One ring buffer and one DAI interconnect only are supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more elements provided by the driver (ring buffers,
  // DAI interconnects and other processing elements), allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  fuchsia_hardware_audio_signalprocessing::Element ring_buffer;
  fuchsia_hardware_audio_signalprocessing::Endpoint ring_buffer_endpoint;
  ring_buffer_endpoint.type(fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer);
  ring_buffer.id(kRingBufferId)
      .type(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint)
      .type_specific(fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint(
          std::move(ring_buffer_endpoint)));

  fuchsia_hardware_audio_signalprocessing::Element dai;
  fuchsia_hardware_audio_signalprocessing::Endpoint dai_endpoint;
  dai_endpoint.type(fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect);
  dai.id(kDaiId)
      .type(fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint)
      .type_specific(fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithEndpoint(
          std::move(dai_endpoint)));

  std::vector elements{std::move(ring_buffer), std::move(dai)};
  completer.Reply(zx::ok(elements));
}

void VirtualAudioComposite::WatchElementState(WatchElementStateRequest& request,
                                              WatchElementStateCompleter::Sync& completer) {
  // One ring buffer and one DAI interconnect only are supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more elements provided by the driver (ring buffers,
  // DAI interconnects and other processing elements), allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  size_t index = 0;
  switch (request.processing_element_id()) {
    case kRingBufferId:
      index = 0;
      break;
    case kDaiId:
      index = 1;
      break;
    default:
      zxlogf(ERROR, "Invalid processing element id %lu, unbinding",
             request.processing_element_id());
      completer.Close(ZX_ERR_INVALID_ARGS);
      return;
  }
  if (!watch_element_replied_[index]) {
    fuchsia_hardware_audio_signalprocessing::ElementState state;
    fuchsia_hardware_audio_signalprocessing::EndpointElementState endpoint;
    fuchsia_hardware_audio_signalprocessing::PlugState plug_state;
    plug_state.plugged(true).plug_state_time(0);
    endpoint.plug_state(std::move(plug_state));
    state.type_specific(
        fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint(
            std::move(endpoint)));
    completer.Reply(std::move(state));
    watch_element_replied_[index] = true;
  } else if (!element_state_completer_[index]) {
    element_state_completer_[index].emplace(completer.ToAsync());
  } else {
    // The client called WatchElementState when another hanging get was pending for the same id.
    // This is an error condition and hence we unbind the channel.
    zxlogf(ERROR, "WatchElementState called when another hanging get was pending, unbinding");
    completer.Close(ZX_ERR_BAD_STATE);
  }
}

void VirtualAudioComposite::SetElementState(SetElementStateRequest& request,
                                            SetElementStateCompleter::Sync& completer) {
  // One ring buffer and one DAI interconnect only are supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more elements provided by the driver (ring buffers,
  // DAI interconnects and other processing elements), allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  uint64_t id = request.processing_element_id();
  if (id != kRingBufferId && id != kDaiId) {
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  completer.Reply(zx::ok());
}

void VirtualAudioComposite::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  // One ring buffer and one DAI interconnect only are supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more topologies allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  fuchsia_hardware_audio_signalprocessing::Topology topology;
  topology.id(kTopologyId);
  fuchsia_hardware_audio_signalprocessing::EdgePair edge;
  edge.processing_element_id_from(kRingBufferId).processing_element_id_to(kDaiId);
  topology.processing_elements_edge_pairs(std::vector({std::move(edge)}));

  completer.Reply(zx::ok(std::vector{std::move(topology)}));
}

void VirtualAudioComposite::SetTopology(SetTopologyRequest& request,
                                        SetTopologyCompleter::Sync& completer) {
  // One ring buffer and one DAI interconnect only are supported by this driver.
  // TODO(fxbug.dev/124865): Add support for more topologies allowing their configuration and
  // observability via the virtual audio FIDL APIs.
  completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
}

}  // namespace virtual_audio

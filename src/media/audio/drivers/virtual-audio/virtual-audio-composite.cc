// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual-audio/virtual-audio-composite.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/device/audio.h>

#include <fbl/algorithm.h>

#include "src/media/audio/drivers/lib/audio-proto-utils/include/audio-proto-utils/format-utils.h"

namespace virtual_audio {

fuchsia_virtualaudio::Configuration VirtualAudioComposite::GetDefaultConfig() {
  constexpr fuchsia_hardware_audio::ElementId kDefaultRingBufferId = 123;
  constexpr fuchsia_hardware_audio::ElementId kDefaultDaiId = 456;
  constexpr fuchsia_hardware_audio::TopologyId kDefaultTopologyId = 789;

  fuchsia_virtualaudio::Configuration config;
  config.device_name("Virtual Audio Composite Device");
  config.manufacturer_name("Fuchsia");
  config.product_name("Virgil v2, a Virtual Volume Vessel");
  config.unique_id(std::array<uint8_t, 16>({1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0}));

  fuchsia_virtualaudio::Composite composite = {};

  // Composite ring buffer.
  fuchsia_virtualaudio::CompositeRingBuffer composite_ring_buffer = {};

  // Ring Buffer.
  fuchsia_virtualaudio::RingBuffer ring_buffer = {};

  // By default we expose a single ring buffer format: 48kHz stereo 16bit.
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

  // By default we expose one DAI format: 48kHz I2S (stereo 16-in-32, 8 bytes/frame total).
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

  composite_dai_interconnect.id(kDefaultDaiId);
  composite_dai_interconnect.dai_interconnect(std::move(dai_interconnect));
  std::vector<fuchsia_virtualaudio::CompositeDaiInterconnect> composite_dai_interconnects = {};
  composite_dai_interconnects.push_back(std::move(composite_dai_interconnect));
  composite.dai_interconnects(std::move(composite_dai_interconnects));

  // Topology with one ring buffer into one DAI interconnect.
  fuchsia_hardware_audio_signalprocessing::Topology topology;
  topology.id(kDefaultTopologyId);
  fuchsia_hardware_audio_signalprocessing::EdgePair edge;

  edge.processing_element_id_from(kDefaultRingBufferId).processing_element_id_to(kDefaultDaiId);
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

zx::result<std::unique_ptr<VirtualAudioComposite>> VirtualAudioComposite::Create(
    InstanceId instance_id, fuchsia_virtualaudio::Configuration config,
    async_dispatcher_t* dispatcher, fidl::ServerEnd<fuchsia_virtualaudio::Device> server,
    OnDeviceBindingClosed on_binding_closed,
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent) {
  auto device = std::make_unique<VirtualAudioComposite>(
      instance_id, std::move(config), dispatcher, std::move(server), std::move(on_binding_closed));
  if (zx::result result = device->Init(parent); result.is_error()) {
    fdf::error("Failed to initialize virtual audio composite device: {}", result);
    return result.take_error();
  }
  return zx::ok(std::move(device));
}

zx::result<> VirtualAudioComposite::Init(
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent) {
  std::string child_node_name = "virtual-audio-composite-" + std::to_string(instance_id_);

  zx::result connector = devfs_connector_.Bind(dispatcher_);
  if (connector.is_error()) {
    fdf::error("Failed to bind devfs connector: {}", connector);
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{
      {.connector = std::move(connector.value()), .class_name{kClassName}}};

  zx::result child =
      fdf::AddOwnedChild(parent, *fdf::Logger::GlobalInstance(), child_node_name, devfs_args);
  if (child.is_error()) {
    fdf::error("Failed to add owned child: {}", child);
    return child.take_error();
  }
  child_.emplace(std::move(child.value()));

  return zx::ok();
}

fuchsia_virtualaudio::RingBuffer& VirtualAudioComposite::GetRingBuffer(uint64_t id) {
  // TODO(https://fxbug.dev/42075676): Add support for a variable number of ring buffers (incl. 0).
  ZX_ASSERT(id == kRingBufferId);
  auto& ring_buffers = config_.device_specific()->composite()->ring_buffers().value();
  ZX_ASSERT(ring_buffers.size() == 1);
  ZX_ASSERT(ring_buffers[0].ring_buffer().has_value());
  return ring_buffers[0].ring_buffer().value();
}

void VirtualAudioComposite::GetFormat(GetFormatCompleter::Sync& completer) {
  if (!ring_buffer_format_.has_value() || !ring_buffer_format_->pcm_format().has_value()) {
    fdf::warn("Ring buffer not initialized");
    completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNoRingBuffer));
    return;
  }

  auto& pcm_format = ring_buffer_format_->pcm_format();
  auto& ring_buffer = GetRingBuffer(kRingBufferId);
  int64_t external_delay = 0;
  if (ring_buffer.external_delay().has_value()) {
    external_delay = ring_buffer.external_delay().value();
  };

  auto sample_format = audio::utils::GetSampleFormat(pcm_format->valid_bits_per_sample(),
                                                     pcm_format->bytes_per_sample() * 8);
  fuchsia_virtualaudio::DeviceGetFormatResponse response{
      {.frames_per_second = pcm_format->frame_rate(),
       .sample_format = sample_format,
       .num_channels = pcm_format->number_of_channels(),
       .external_delay = external_delay}};
  completer.Reply(fit::ok(std::move(response)));
}

void VirtualAudioComposite::GetBuffer(GetBufferCompleter::Sync& completer) {
  if (!ring_buffer_vmo_.is_valid()) {
    fdf::warn("Ring buffer not initialized");
    completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNoRingBuffer));
    return;
  }

  zx::vmo dup_vmo;
  zx_status_t status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &dup_vmo);
  if (status != ZX_OK) {
    fdf::error("Failed to create ring buffer: {}", zx_status_get_string(status));
    completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNoRingBuffer));
    return;
  }

  fuchsia_virtualaudio::DeviceGetBufferResponse response{{
      .ring_buffer = std::move(dup_vmo),
      .num_ring_buffer_frames = num_ring_buffer_frames_,
      .notifications_per_ring = notifications_per_ring_,
  }};
  completer.Reply(fit::ok(std::move(response)));
}

// Health implementation
//
void VirtualAudioComposite::GetHealthState(GetHealthStateCompleter::Sync& completer) {
  completer.Reply(fuchsia_hardware_audio::HealthState{}.healthy(true));
}

// Composite implementation
//
void VirtualAudioComposite::Reset(ResetCompleter::Sync& completer) {
  // Must clear all state for DAIs.
  // Must stop all RingBuffers, close connections and clear all state for RingBuffers elements.
  // Must clear all state for signalprocessing elements.
  // Must clear all signalprocessing topology state (presumably returning to a default topology?)

  completer.Reply(zx::ok());
}

void VirtualAudioComposite::GetProperties(
    fidl::Server<fuchsia_hardware_audio::Composite>::GetPropertiesCompleter::Sync& completer) {
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
  // This driver is limited to a single DAI interconnect.
  // TODO(https://fxbug.dev/42075676): Add support for more DAI interconnects, enabling
  // configuration and observability in the virtual_audio FIDL API.
  if (request.processing_element_id() != kDaiId) {
    fdf::error("GetDaiFormats({}) bad element_id", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
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
  // This driver is limited to a single DAI interconnect.
  // TODO(https://fxbug.dev/42075676): Add support for more DAI interconnects, enabling
  // configuration and observability in the virtual_audio FIDL API.
  if (request.processing_element_id() != kDaiId) {
    fdf::error("SetDaiFormat({}) bad element_id", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  fuchsia_hardware_audio::DaiFormat format = request.format();
  if (format.frame_rate() > 192000) {
    fdf::error("SetDaiFormat frame_rate ({}) too high", format.frame_rate());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }

  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> supported_formats{};
  if (composite_config().dai_interconnects() && !composite_config().dai_interconnects()->empty() &&
      composite_config().dai_interconnects()->at(0).dai_interconnect() &&
      composite_config().dai_interconnects()->at(0).dai_interconnect()->dai_supported_formats()) {
    supported_formats = composite_config()
                            .dai_interconnects()
                            ->at(0)
                            .dai_interconnect()
                            ->dai_supported_formats()
                            .value();
  }

  for (auto dai_format_set : supported_formats) {
    std::optional<uint32_t> number_of_channels;
    for (auto channel_count : dai_format_set.number_of_channels()) {
      if (channel_count == format.number_of_channels()) {
        number_of_channels = format.number_of_channels();
        break;
      }
    }
    std::optional<uint64_t> channels_to_use_bitmask;
    if (format.channels_to_use_bitmask() <= (1u << format.number_of_channels()) - 1) {
      channels_to_use_bitmask = format.channels_to_use_bitmask();
    }
    std::optional<fuchsia_hardware_audio::DaiSampleFormat> sample_format;
    for (auto sample_fmt : dai_format_set.sample_formats()) {
      if (sample_fmt == format.sample_format()) {
        sample_format = format.sample_format();
        break;
      }
    }
    std::optional<fuchsia_hardware_audio::DaiFrameFormat> frame_format;
    for (auto& frame_fmt : dai_format_set.frame_formats()) {
      if (frame_fmt == format.frame_format()) {
        frame_format = format.frame_format();
        break;
      }
    }
    std::optional<uint32_t> frame_rate;
    for (auto rate : dai_format_set.frame_rates()) {
      if (rate == format.frame_rate()) {
        frame_rate = format.frame_rate();
        break;
      }
    }
    std::optional<uint8_t> bits_per_slot;
    for (auto bits : dai_format_set.bits_per_slot()) {
      if (bits == format.bits_per_slot()) {
        bits_per_slot = format.bits_per_slot();
        break;
      }
    }
    std::optional<uint8_t> bits_per_sample;
    for (auto bits : dai_format_set.bits_per_sample()) {
      if (bits == format.bits_per_sample()) {
        bits_per_sample = format.bits_per_sample();
        break;
      }
    }
    if (number_of_channels.has_value() && channels_to_use_bitmask.has_value() &&
        sample_format.has_value() && frame_format.has_value() && frame_rate.has_value() &&
        bits_per_slot.has_value() && bits_per_sample.has_value()) {
      completer.Reply(zx::ok());
      return;
    }
  }
  fdf::error("SetDaiFormat: unsupported format");
  completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
}

void VirtualAudioComposite::GetRingBufferFormats(GetRingBufferFormatsRequest& request,
                                                 GetRingBufferFormatsCompleter::Sync& completer) {
  // This driver is limited to a single ring buffer.
  // TODO(https://fxbug.dev/42075676): Add support for more ring buffers, enabling configuration and
  // observability in the virtual_audio FIDL API.
  if (request.processing_element_id() != kRingBufferId) {
    fdf::error("GetRingBufferFormats({}) bad element_id", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
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
  // Do not log canceled cases; these happen particularly frequently in certain test cases.
  if (info.status() != ZX_ERR_CANCELED) {
    fdf::info("Ring buffer channel closing: {}", info.FormatDescription().c_str());
  }
  ResetRingBuffer();
}

void VirtualAudioComposite::CreateRingBuffer(CreateRingBufferRequest& request,
                                             CreateRingBufferCompleter::Sync& completer) {
  // One ring buffer is supported by this driver.
  // TODO(https://fxbug.dev/42075676): Add support for more ring buffers, enabling configuration and
  // observability in the virtual_audio FIDL API.
  if (request.processing_element_id() != kRingBufferId) {
    fdf::error("CreateRingBuffer({}) bad element_id", request.processing_element_id());
    completer.Reply(zx::error(fuchsia_hardware_audio::DriverError::kInvalidArgs));
    return;
  }
  ring_buffer_format_.emplace(std::move(request.format()));
  ring_buffer_active_channel_mask_ =
      (1 << ring_buffer_format_->pcm_format()->number_of_channels()) - 1;
  active_channel_set_time_ = zx::clock::get_monotonic();
  ring_buffer_.emplace(dispatcher_, std::move(request.ring_buffer()), this,
                       std::mem_fn(&VirtualAudioComposite::OnRingBufferClosed));
  completer.Reply(zx::ok());
}

void VirtualAudioComposite::ResetRingBuffer() {
  ring_buffer_vmo_fetched_ = false;
  ring_buffer_started_ = false;
  notifications_per_ring_ = 0;
  should_reply_to_position_request_ = true;
  position_info_completer_.reset();
  should_reply_to_delay_request_ = true;
  delay_info_completer_.reset();
  // We don't reset ring_buffer_format_ and dai_format_ to allow for retrieval for observability.
}

// RingBuffer implementation
//
void VirtualAudioComposite::GetProperties(
    fidl::Server<fuchsia_hardware_audio::RingBuffer>::GetPropertiesCompleter::Sync& completer) {
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
  // The ring buffer must be at least min_frames + fifo_frames.
  num_ring_buffer_frames_ =
      request.min_frames() +
      (ring_buffer.driver_transfer_bytes().value() + frame_size_ - 1) / frame_size_;

  num_ring_buffer_frames_ = std::max(
      min_frames, fbl::round_up<uint32_t, uint32_t>(num_ring_buffer_frames_, modulo_frames));

  zx_status_t status = ring_buffer_mapper_.CreateAndMap(
      static_cast<uint64_t>(num_ring_buffer_frames_) * frame_size_,
      ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &ring_buffer_vmo_,
      ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP | ZX_RIGHT_DUPLICATE | ZX_RIGHT_TRANSFER);

  ZX_ASSERT_MSG(status == ZX_OK, "failed to create ring buffer VMO: %s",
                zx_status_get_string(status));

  zx::vmo out_vmo;
  zx_rights_t required_rights = ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_MAP;
  if (ring_buffer_is_outgoing_) {
    required_rights |= ZX_RIGHT_WRITE;
  }
  status = ring_buffer_vmo_.duplicate(required_rights, &out_vmo);
  ZX_ASSERT_MSG(status == ZX_OK, "failed to duplicate VMO handle for out param: %s",
                zx_status_get_string(status));

  notifications_per_ring_ = request.clock_recovery_notifications_per_ring();

  zx::vmo duplicate_vmo_for_va;
  status = ring_buffer_vmo_.duplicate(
      ZX_RIGHT_TRANSFER | ZX_RIGHT_READ | ZX_RIGHT_WRITE | ZX_RIGHT_MAP, &duplicate_vmo_for_va);
  ZX_ASSERT_MSG(status == ZX_OK, "failed to duplicate VMO handle for VA client: %s",
                zx_status_get_string(status));

  fidl::Status result = fidl::WireSendEvent(device_binding_)
                            ->OnBufferCreated(std::move(duplicate_vmo_for_va),
                                              num_ring_buffer_frames_, notifications_per_ring_);
  if (result.status() != ZX_OK) {
    fdf::warn("Failed to send OnBufferCreated event: {}", result);
  }

  fuchsia_hardware_audio::RingBufferGetVmoResponse response;
  response.num_frames(num_ring_buffer_frames_);
  response.ring_buffer(std::move(out_vmo));
  completer.Reply(zx::ok(std::move(response)));
  ring_buffer_vmo_fetched_ = true;
}

void VirtualAudioComposite::Start(StartCompleter::Sync& completer) {
  if (!ring_buffer_vmo_fetched_) {
    fdf::error("Cannot start the ring buffer before retrieving the VMO");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (ring_buffer_started_) {
    fdf::error("Cannot start the ring buffer if already started");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  zx_time_t now = zx::clock::get_monotonic().get();

  fidl::Status result = fidl::WireSendEvent(device_binding_)->OnStart(now);
  if (result.status() != ZX_OK) {
    fdf::warn("Failed to send OnStart event: {}", result);
  }

  ring_buffer_started_ = true;
  completer.Reply(now);
}

void VirtualAudioComposite::Stop(StopCompleter::Sync& completer) {
  if (!ring_buffer_vmo_fetched_) {
    fdf::error("Cannot start the ring buffer before retrieving the VMO");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }
  if (!ring_buffer_started_) {
    fdf::info("Stop called while stopped; doing nothing");
    completer.Reply();
    return;
  }
  zx_time_t now = zx::clock::get_monotonic().get();
  // TODO(https://fxbug.dev/42075676): Add support for 'stop' position, now we always report 0.
  fidl::Status result = fidl::WireSendEvent(device_binding_)->OnStop(now, 0);
  if (result.status() != ZX_OK) {
    fdf::warn("Failed to send OnStop event: {}", result);
  }

  ring_buffer_started_ = false;
  completer.Reply();
}

void VirtualAudioComposite::WatchClockRecoveryPositionInfo(
    WatchClockRecoveryPositionInfoCompleter::Sync& completer) {
  if (should_reply_to_position_request_ && ring_buffer_started_ && notifications_per_ring_ > 0) {
    fuchsia_hardware_audio::RingBufferPositionInfo position_info;
    position_info.timestamp(zx::clock::get_monotonic().get());
    // TODO(https://fxbug.dev/42075676): Add support for current position; now we always report 0.
    position_info.position(0);
    should_reply_to_position_request_ = false;
    completer.Reply(std::move(position_info));
    return;
  }

  if (position_info_completer_.has_value()) {
    // The client called WatchClockRecoveryPositionInfo when another hanging get was pending.
    fdf::error("WatchClockRecoveryPositionInfo called while previous call was pending. Unbinding");
    should_reply_to_position_request_ = true;
    position_info_completer_.reset();
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  position_info_completer_.emplace(completer.ToAsync());
}

void VirtualAudioComposite::WatchDelayInfo(WatchDelayInfoCompleter::Sync& completer) {
  if (should_reply_to_delay_request_) {
    auto& ring_buffer = GetRingBuffer(kRingBufferId);
    fuchsia_hardware_audio::DelayInfo delay_info;
    delay_info.internal_delay(ring_buffer.internal_delay());
    delay_info.external_delay(ring_buffer.external_delay());
    should_reply_to_delay_request_ = false;
    completer.Reply(std::move(delay_info));
  } else if (!delay_info_completer_.has_value()) {
    delay_info_completer_.emplace(completer.ToAsync());
  } else {
    // The client called WatchDelayInfo when another hanging get was pending.
    fdf::error("WatchDelayInfo called while previous call was pending. Unbinding");
    should_reply_to_delay_request_ = true;
    delay_info_completer_.reset();
    completer.Close(ZX_ERR_BAD_STATE);
  }
}

void VirtualAudioComposite::SetActiveChannels(
    fuchsia_hardware_audio::RingBufferSetActiveChannelsRequest& request,
    SetActiveChannelsCompleter::Sync& completer) {
  ZX_ASSERT(ring_buffer_format_);  // A RingBuffer must exist, for this FIDL method to be called.

  const uint64_t max_channel_bitmask =
      (1 << ring_buffer_format_->pcm_format()->number_of_channels()) - 1;
  if (request.active_channels_bitmask() > max_channel_bitmask) {
    fdf::warn("SetActiveChannels({:#016x}) is out-of-range", request.active_channels_bitmask());
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  if (ring_buffer_active_channel_mask_ != request.active_channels_bitmask()) {
    active_channel_set_time_ = zx::clock::get_monotonic();
    ring_buffer_active_channel_mask_ = request.active_channels_bitmask();
  }
  completer.Reply(zx::ok(active_channel_set_time_.get()));
}

// signalprocessing
//
void VirtualAudioComposite::SignalProcessingConnect(
    SignalProcessingConnectRequest& request, SignalProcessingConnectCompleter::Sync& completer) {
  if (signal_) {
    fdf::error("Signal processing already bound");
    request.protocol().Close(ZX_ERR_ALREADY_BOUND);
    return;
  }

  SetupSignalProcessing();
  signal_.emplace(dispatcher_, std::move(request.protocol()), this,
                  std::mem_fn(&VirtualAudioComposite::OnSignalProcessingClosed));
}

void VirtualAudioComposite::OnSignalProcessingClosed(fidl::UnbindInfo info) {
  if (info.is_peer_closed()) {
    fdf::info("Client disconnected");
  } else if (!info.is_user_initiated()) {
    // Do not log canceled cases; these happen particularly frequently in certain test cases.
    if (info.status() != ZX_ERR_CANCELED) {
      fdf::error("Client connection unbound: {}", info.status_string());
    }
  }
  if (signal_) {
    signal_.reset();
  }
  for (auto& [_element_id, snapshot] : element_states_) {
    snapshot.completer.reset();
    snapshot.last_notified.reset();
  }
  last_reported_topology_id_.reset();
  watch_topology_completer_.reset();
}

void VirtualAudioComposite::SetupSignalProcessing() {
  SetupSignalProcessingElements();
  SetupSignalProcessingTopologies();
  SetupSignalProcessingElementStates();
}

// signalprocessing Element handling
//
// This driver is limited to a ring buffer, a DAI interconnect and a gain element.
// TODO(https://fxbug.dev/42075676): Add support for more elements provided by the driver
// (additional processing element types), enabling configuration and observability via the
// virtual_audio FIDL API.
void VirtualAudioComposite::SetupSignalProcessingElements() {
  element_map_.clear();
  elements_.clear();

  fuchsia_hardware_audio_signalprocessing::Element ring_buffer;
  ring_buffer.id(kRingBufferId)
      .type(fuchsia_hardware_audio_signalprocessing::ElementType::kRingBuffer);

  fuchsia_hardware_audio_signalprocessing::Element dai;
  fuchsia_hardware_audio_signalprocessing::DaiInterconnect dai_interconnect;
  // Connect this to the existing virtualaudio FIDL method for dynamic plug_state changes?
  dai_interconnect.plug_detect_capabilities(
      fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired);
  dai.id(kDaiId)
      .type(fuchsia_hardware_audio_signalprocessing::ElementType::kDaiInterconnect)
      .type_specific(
          fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithDaiInterconnect(
              std::move(dai_interconnect)));

  fuchsia_hardware_audio_signalprocessing::Element gain;
  fuchsia_hardware_audio_signalprocessing::Gain gain_type_specific;
  gain_type_specific.type(fuchsia_hardware_audio_signalprocessing::GainType::kDecibels)
      .domain(fuchsia_hardware_audio_signalprocessing::GainDomain::kDigital)
      .min_gain(-68.0)
      .max_gain(+6.0)
      .min_gain_step(0.5);
  gain.id(kGainId)
      .type(fuchsia_hardware_audio_signalprocessing::ElementType::kGain)
      .type_specific(fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::WithGain(
          gain_type_specific));

  elements_ = {{ring_buffer, dai, gain}};
  for (auto& el : elements_) {
    element_map_.insert({*el.id(), &el});
  }
}

void VirtualAudioComposite::GetElements(GetElementsCompleter::Sync& completer) {
  completer.Reply(zx::ok(elements_));
}

// signalprocessing Topology handling
//
// We expose two topologies: { Rb -> Gain -> Dai } and { Dai -> Gain -> Rb }.
// TODO(https://fxbug.dev/42075676): Add more complex topologies, including elements that are only
// in some topologies (not all). Include signalprocessing configuration/observability in the
// virtual_audio FIDL API.
void VirtualAudioComposite::SetupSignalProcessingTopologies() {
  topologies_.clear();
  fuchsia_hardware_audio_signalprocessing::Topology topology1, topology2;
  fuchsia_hardware_audio_signalprocessing::EdgePair edge1, edge2;

  topology1.id(kPlaybackTopologyId);
  edge1.processing_element_id_from(kRingBufferId).processing_element_id_to(kGainId);
  edge2.processing_element_id_from(kGainId).processing_element_id_to(kDaiId);
  topology1.processing_elements_edge_pairs(std::vector({std::move(edge1), std::move(edge2)}));

  topology2.id(kCaptureTopologyId);
  edge1.processing_element_id_from(kDaiId).processing_element_id_to(kGainId);
  edge2.processing_element_id_from(kGainId).processing_element_id_to(kRingBufferId);
  topology2.processing_elements_edge_pairs(std::vector({std::move(edge1), std::move(edge2)}));

  // By default (in the topology of kPlaybackTopologyId), our ring buffer will be an outgoing one.
  ring_buffer_is_outgoing_ = true;
  current_topology_id_ = kPlaybackTopologyId;
  topologies_.emplace_back(std::move(topology1));
  topologies_.emplace_back(std::move(topology2));
}

void VirtualAudioComposite::GetTopologies(GetTopologiesCompleter::Sync& completer) {
  completer.Reply(zx::ok(topologies_));
}

void VirtualAudioComposite::SetTopology(SetTopologyRequest& request,
                                        SetTopologyCompleter::Sync& completer) {
  if (request.topology_id() != kPlaybackTopologyId && request.topology_id() != kCaptureTopologyId) {
    fdf::error("SetTopology({}) unknown topology_id", request.topology_id());
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  ring_buffer_is_outgoing_ = (request.topology_id() == kPlaybackTopologyId);
  current_topology_id_ = request.topology_id();
  completer.Reply(zx::ok());
  MaybeCompleteWatchTopology();
}

void VirtualAudioComposite::WatchTopology(WatchTopologyCompleter::Sync& completer) {
  // The client should not call WatchTopology when a previous WatchTopology is pending.
  if (watch_topology_completer_.has_value()) {
    fdf::error("WatchTopology called while previous call was pending. Unbinding");
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  watch_topology_completer_ = completer.ToAsync();
  MaybeCompleteWatchTopology();
}

// If we should tell the client about the topology, and if there is a pending request, complete it.
void VirtualAudioComposite::MaybeCompleteWatchTopology() {
  if (watch_topology_completer_.has_value() &&
      (!last_reported_topology_id_.has_value() ||
       *last_reported_topology_id_ != current_topology_id_)) {
    last_reported_topology_id_ = current_topology_id_;
    auto completer = std::move(watch_topology_completer_);
    watch_topology_completer_.reset();
    completer->Reply(current_topology_id_);
  }
}

// signalprocessing ElementState handling
//
// This driver is limited to a ring buffer, a DAI interconnect and a gain element. Of these, DAI and
// Gain return type-specific ElementState; only the Gain element has _settable_ type-specific state.
// TODO(https://fxbug.dev/42075676): Add support for diverse element types, as well as dynamic
// (unsolicited) state changes, with complex state that can be configured and observed via the
// virtual_audio FIDL API.
void VirtualAudioComposite::SetupSignalProcessingElementStates() {
  element_states_.clear();

  fuchsia_hardware_audio_signalprocessing::DaiInterconnectElementState dai_state;
  fuchsia_hardware_audio_signalprocessing::PlugState plug_state;
  plug_state.plugged(true).plug_state_time(0);
  dai_state.plug_state(std::move(plug_state));
  ElementSnapshot dai_snapshot;
  dai_snapshot.current.started(true).bypassed(false).type_specific(
      fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithDaiInterconnect(
          std::move(dai_state)));
  dai_snapshot.last_notified.reset();
  dai_snapshot.completer.reset();
  element_states_.insert({kDaiId, std::move(dai_snapshot)});

  ElementSnapshot rb_snapshot;
  rb_snapshot.current.started(true).bypassed(false).processing_delay(0);
  rb_snapshot.last_notified.reset();
  rb_snapshot.completer.reset();
  element_states_.insert({kRingBufferId, std::move(rb_snapshot)});

  fuchsia_hardware_audio_signalprocessing::GainElementState gain_state;
  gain_state.gain(-6.0);
  ElementSnapshot gain_snapshot;
  gain_snapshot.current.started(true).bypassed(false).turn_on_delay(0).type_specific(
      fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithGain(gain_state));
  gain_snapshot.last_notified.reset();
  gain_snapshot.completer.reset();
  element_states_.insert({kGainId, std::move(gain_snapshot)});
}

// Note that the range of type-specific state for an element is greater than the range of
// type-specific state that can be changed by clients. This is why we define two distinct unions:
//
// TypeSpecificElementState is used by the method WatchElementState.
// This union defines variants for DAI, DYNAMICS, EQUALIZER, GAIN and VENDOR_SPECIFIC element types.
//
// SettableTypeSpecificElementState is used by the method SetElementState.
// This union defines variants for DYNAMICS, EQUALIZER, GAIN and VENDOR_SPECIFIC element types.
//
// To verify these modes, the driver supports 1 ring buffer, 1 gain element and 1 DAI interconnect.
// TODO(https://fxbug.dev/42075676): Add support for more elements specified in the Configuration,
// enabling dynamic behavior and observability via the virtual_audio FIDL API.
void VirtualAudioComposite::SetElementState(SetElementStateRequest& request,
                                            SetElementStateCompleter::Sync& completer) {
  fuchsia_hardware_audio::ElementId element_id = request.processing_element_id();

  // Reject all error cases BEFORE changing any state variables.

  // Error: unknown element_id
  if (!element_map_.contains(element_id)) {
    fdf::error("SetElementState({}) unknown element_id", element_id);
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  const auto& ele = element_map_[element_id];

  // Error: this element cannot Stop as requested.
  if (request.state().started().has_value() && !(*request.state().started()) &&
      !ele->can_stop().value_or(false)) {
    fdf::error("SetElementState: element cannot be stopped");
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  // Error: this element cannot Bypass as requested.
  if (request.state().bypassed().has_value() && *request.state().bypassed() &&
      !ele->can_bypass().value_or(false)) {
    fdf::error("SetElementState: element cannot be bypassed");
    completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  switch (element_id) {
    case kRingBufferId:
      // RING_BUFFER elements contain no type-specific state.
      // TypeSpecificElementState contains no variant for the RING_BUFFER type.
      __FALLTHROUGH;
    case kDaiId:
      // DAI_INTERCONNECT elements can specify type-specific state, but clients cannot CHANGE it.
      // TypeSpecificElementState defines a DAI variant; SettableTypeSpecificElementState does not.
      if (request.state().type_specific().has_value()) {
        // Error: type_specific state in this request does not match this element type.
        fdf::error("SetElementState: TypeSpecificElementState does not match this element type");
        completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
        return;
      }
      break;
    case kGainId:
      if (request.state().type_specific().has_value()) {
        // For this element, clients can specify type-specific state but it must be gain-specific.
        if (!request.state().type_specific()->gain().has_value()) {
          // Error: type_specific state in this request does not match this element type.
          fdf::error("SetElementState: TypeSpecificElementState does not match this element type");
          completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
          return;
        }
        // Error: SetElementState value is missing or non-finite.
        if (!request.state().type_specific()->gain()->gain().has_value() ||
            !std::isfinite(*request.state().type_specific()->gain()->gain())) {
          fdf::error("SetElementState: Gain requires a finite gain");
          completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
          return;
        }
        // Error: SetElementState value is outside the [min,max] bounds.
        if (*request.state().type_specific()->gain()->gain() <
                *element_map_[element_id]->type_specific()->gain()->min_gain() ||
            *request.state().type_specific()->gain()->gain() >
                *element_map_[element_id]->type_specific()->gain()->max_gain()) {
          fdf::error("SetElementState: Gain {} is outside the allowed range [{}, {}]",
                     *request.state().type_specific()->gain()->gain(),
                     *element_map_[element_id]->type_specific()->gain()->min_gain(),
                     *element_map_[element_id]->type_specific()->gain()->max_gain());
          completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
          return;
        }

        // We passed every check so we can record this state change. First: type-specific changes.
        element_states_.at(element_id)
            .current.type_specific(
                fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithGain(
                    request.state().type_specific()->gain().value()));
      }
      break;
    default:
      // Error: we don't recognize this element_id.
      fdf::error("SetElementState({}) unknown element_id", element_id);
      completer.Reply(zx::error(ZX_ERR_INVALID_ARGS));
      return;
  }

  // All error cases have exited. Record the non-type-specific state changes, if any.
  if (request.state().started().has_value()) {
    element_states_.at(element_id).current.started() = request.state().started();
  }
  if (request.state().bypassed().has_value()) {
    element_states_.at(element_id).current.bypassed() = request.state().bypassed();
  }
  if (request.state().vendor_specific_data().has_value()) {
    fdf::warn("SetElementState({}): ignoring {} bytes of vendor_specific_data (unsupported)",
              element_id, request.state().vendor_specific_data()->size());
  }

  completer.Reply(zx::ok());
  MaybeCompleteWatchElementState(element_id);
}

// Immediately return the state of this element, if it has changed since last time this was called.
// Otherwise, pend this call until the state DOES change.
void VirtualAudioComposite::WatchElementState(WatchElementStateRequest& request,
                                              WatchElementStateCompleter::Sync& completer) {
  if (!element_states_.contains(request.processing_element_id())) {
    fdf::error("WatchElementState({}) unknown element_id. Unbinding",
               request.processing_element_id());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (element_states_.at(request.processing_element_id()).completer.has_value()) {
    // The client called WatchElementState when another hanging get was pending for the same id.
    fdf::error("WatchElementState({}) called while previous call was pending. Unbinding",
               request.processing_element_id());
    completer.Close(ZX_ERR_BAD_STATE);
    return;
  }

  element_states_.at(request.processing_element_id()).completer = completer.ToAsync();
  MaybeCompleteWatchElementState(request.processing_element_id());
}

// WatchElementState or SetElementState were called for this element (or it changed state for some
// other reason). If there is a pending WatchElementState to complete, do so.
void VirtualAudioComposite::MaybeCompleteWatchElementState(
    fuchsia_hardware_audio_signalprocessing::ElementId element_id) {
  if (!element_states_.at(element_id).completer.has_value()) {
    fdf::debug("We don't have a completer, so we can't complete this");
    return;
  }
  const auto& prev = element_states_.at(element_id).last_notified;
  const auto& curr = element_states_.at(element_id).current;
  if (prev.has_value() && prev->type_specific() == curr.type_specific() &&
      prev->started().value_or(true) == curr.started().value_or(true) &&
      prev->bypassed().value_or(false) == curr.bypassed().value_or(false)) {
    fdf::debug("The value is unchanged, so we won't complete this");
    return;
  }

  auto completer = std::move(element_states_.at(element_id).completer);
  element_states_.at(element_id).completer.reset();
  element_states_.at(element_id).last_notified = element_states_.at(element_id).current;
  completer->Reply(element_states_.at(element_id).current);
}

// Driver doesn't support a new RingBuffer method. Complain loudly but don't disconnect, since
// this test fixture might be used with a client that is built with a newer SDK version.
void VirtualAudioComposite::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_audio::RingBuffer> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  fdf::error("VirtualAudioComposite::handle_unknown_method (RingBuffer) ordinal {}",
             metadata.method_ordinal);
}

// Driver doesn't support a new SignalProcessing method. Complain loudly but don't disconnect, since
// this test fixture might be used with a client that is built with a newer SDK version.
void VirtualAudioComposite::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_audio_signalprocessing::SignalProcessing> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  fdf::error("VirtualAudioComposite::handle_unknown_method (SignalProcessing) ordinal {}",
             metadata.method_ordinal);
}

void VirtualAudioComposite::Serve(fidl::ServerEnd<fuchsia_hardware_audio::Composite> server) {
  if (composite_binding_.has_value()) {
    fdf::error("Already bound");
    server.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }
  composite_binding_.emplace(dispatcher_, std::move(server), this,
                             [this](auto info) { composite_binding_.reset(); });
}

void VirtualAudioComposite::GetGain(GetGainCompleter::Sync& completer) {
  completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNotSupported));
}

void VirtualAudioComposite::SetNotificationFrequency(
    SetNotificationFrequencyRequest& request, SetNotificationFrequencyCompleter::Sync& completer) {
  completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNotSupported));
}

void VirtualAudioComposite::GetPosition(GetPositionCompleter::Sync& completer) {
  completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNotSupported));
}

void VirtualAudioComposite::ChangePlugState(ChangePlugStateRequest& request,
                                            ChangePlugStateCompleter::Sync& completer) {
  completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNotSupported));
}

void VirtualAudioComposite::AdjustClockRate(AdjustClockRateRequest& request,
                                            AdjustClockRateCompleter::Sync& completer) {
  completer.Reply(fit::error(fuchsia_virtualaudio::Error::kNotSupported));
}

}  // namespace virtual_audio

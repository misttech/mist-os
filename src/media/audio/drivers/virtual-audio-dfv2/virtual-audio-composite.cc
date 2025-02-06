// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/media/audio/drivers/virtual-audio-dfv2/virtual-audio-composite.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <zircon/device/audio.h>

namespace virtual_audio {

fuchsia_virtualaudio::Configuration VirtualAudioComposite::GetDefaultConfig() {
  constexpr fuchsia_hardware_audio::ElementId kDefaultRingBufferId = 123;
  constexpr fuchsia_hardware_audio::ElementId kDefaultDaiId = 456;
  constexpr fuchsia_hardware_audio::TopologyId kDefaultTopologyId = 789;

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

}  // namespace virtual_audio

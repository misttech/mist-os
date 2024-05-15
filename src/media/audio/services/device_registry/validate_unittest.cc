// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/validate.h"

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>

#include <optional>
#include <vector>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/signal_processing_utils.h"
#include "src/media/audio/services/device_registry/signal_processing_utils_unittest.h"

namespace media_audio {
namespace {

namespace fha = fuchsia_hardware_audio;

// These cases unittest the Validate... functions with inputs that cause INFO logging (if any).

const std::vector<uint8_t> kChannels = {1, 8, 255};
const std::vector<std::pair<uint8_t, fha::SampleFormat>> kFormats = {
    {1, fha::SampleFormat::kPcmUnsigned}, {2, fha::SampleFormat::kPcmSigned},
    {4, fha::SampleFormat::kPcmSigned},   {4, fha::SampleFormat::kPcmFloat},
    {8, fha::SampleFormat::kPcmFloat},
};
const std::vector<uint32_t> kFrameRates = {1000, 44100, 48000, 19200};

// Unittest ValidateCodecProperties -- the missing, minimal and maximal possibilities
TEST(ValidateTest, ValidateCodecProperties) {
  EXPECT_TRUE(ValidateCodecProperties(fha::CodecProperties{{
      // is_input missing
      .manufacturer = " ",  // minimal value (empty is disallowed)
      .product =            // maximal value
      "Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name extends to 321X",
      // unique_id missing
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
  }}));
  EXPECT_TRUE(ValidateCodecProperties(fha::CodecProperties{{
      .is_input = false,
      .manufacturer =  // maximal value
      "Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long, which extends to... 321X",
      // product missing
      .unique_id = {{}},  // minimal value
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kCanAsyncNotify,
  }}));
  EXPECT_TRUE(ValidateCodecProperties(fha::CodecProperties{{
      .is_input = true,
      // manufacturer missing
      .product = " ",  // minimal value (empty is disallowed)
      .unique_id = {{0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  //
                     0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF}},
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kCanAsyncNotify,
  }}));
}

// Unittest ValidateCompositeProperties -- the missing, minimal and maximal possibilities
TEST(ValidateTest, ValidateCompositeProperties) {
  EXPECT_TRUE(ValidateCompositeProperties(fha::CompositeProperties{{
      // manufacturer missing
      .product = " ",  // minimal value (empty is disallowed)
      .unique_id = {{0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  //
                     0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF}},
      .clock_domain = fha::kClockDomainMonotonic,
  }}));
  EXPECT_TRUE(ValidateCompositeProperties(fha::CompositeProperties{{
      .manufacturer = " ",  // minimal value (empty is disallowed)
      .product =            // maximal value
      "Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name extends to 321X",
      // unique_id missing
      .clock_domain = fha::kClockDomainExternal,
  }}));
  EXPECT_TRUE(ValidateCompositeProperties(fha::CompositeProperties{{
      .manufacturer =  // maximal value
      "Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long, which extends to... 321X",
      // product missing
      .unique_id = {{}},  // minimal value
      .clock_domain = fha::kClockDomainExternal,
  }}));
}

TEST(ValidateTest, ValidateStreamProperties) {
  fha::StreamProperties stream_properties{{
      .is_input = false,
      .min_gain_db = 0.0f,
      .max_gain_db = 0.0f,
      .gain_step_db = 0.0f,
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
      .clock_domain = fha::kClockDomainMonotonic,
  }};
  fha::GainState gain_state{{.gain_db = 0.0f}};
  fha::PlugState plug_state{{.plugged = true, .plug_state_time = 0}};

  EXPECT_TRUE(ValidateStreamProperties(stream_properties));
  EXPECT_TRUE(ValidateStreamProperties(stream_properties, gain_state));
  EXPECT_TRUE(ValidateStreamProperties(stream_properties, gain_state, plug_state));

  stream_properties = {{
      .unique_id = {{255, 255, 255, 255, 255, 255, 255, 255,  //
                     255, 255, 255, 255, 255, 255, 255, 255}},
      .is_input = true,
      .can_mute = true,
      .can_agc = true,
      .min_gain_db = -10.0f,
      .max_gain_db = 10.0f,
      .gain_step_db = 20.0f,
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kCanAsyncNotify,
      .manufacturer = "Fuchsia2",
      .product = "Test2",
      .clock_domain = fha::kClockDomainExternal,
  }};
  gain_state = {{.muted = true, .agc_enabled = true, .gain_db = -4.2f}};
  plug_state = {{.plugged = false, .plug_state_time = zx::clock::get_monotonic().get()}};

  EXPECT_TRUE(ValidateStreamProperties(stream_properties));
  EXPECT_TRUE(ValidateStreamProperties(stream_properties, gain_state));
  EXPECT_TRUE(ValidateStreamProperties(stream_properties, gain_state, plug_state));
}

// TODO(https://fxbug.dev/42069012): Unittest ValidateDeviceInfo, incl. manuf & prod 256 chars long

fha::SupportedFormats CompliantFormatSet() {
  return fha::SupportedFormats{{
      .pcm_supported_formats = fha::PcmSupportedFormats{{
          .channel_sets = {{
              fha::ChannelSet{{
                  .attributes = {{
                      fha::ChannelAttributes{{
                          .min_frequency = 20,
                          .max_frequency = 20000,
                      }},
                  }},
              }},
          }},
          .sample_formats = {{fha::SampleFormat::kPcmSigned}},
          .bytes_per_sample = {{2}},
          .valid_bits_per_sample = {{16}},
          .frame_rates = {{48000}},
      }},
  }};
}

TEST(ValidateTest, ValidateRingBufferProperties) {
  EXPECT_TRUE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = false,
      .turn_on_delay = 125,
      .driver_transfer_bytes = 32,
  }}));

  // TODO(b/311694769): Resolve driver_transfer_bytes lower limit: specifically is 0 allowed?
  EXPECT_TRUE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      .turn_on_delay = 0,  // can be zero
      .driver_transfer_bytes = 1,
  }}));

  // TODO(b/311694769): Resolve driver_transfer_bytes upper limit: no limit? Soft guideline?
  EXPECT_TRUE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      // turn_on_delay (optional) is missing
      .driver_transfer_bytes = 128,
  }}));
}

TEST(ValidateTest, ValidateGainState) {
  EXPECT_TRUE(ValidateGainState(fha::GainState{{
      // muted (optional) is missing: NOT muted
      // agc_enabled (optional) is missing: NOT enabled
      .gain_db = -42.0f,
  }}  // If StreamProperties is not provided, assume the device cannot mute or agc
                                ));
  EXPECT_TRUE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = 0,
                        }},
                        // If StreamProperties is not provided, assume the device cannot mute or agc
                        std::nullopt));
  EXPECT_TRUE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = true,
                            .gain_db = -12.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            // can_mute (optional) is missing: device cannot mute
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_TRUE(
      ValidateGainState(fha::GainState{{
                            .muted = true,
                            .agc_enabled = false,
                            .gain_db = -12.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            // can_agc (optional) is missing: device cannot agc
                            .min_gain_db = -24.0f,
                            .max_gain_db = -12.0f,
                            .gain_step_db = 2.0f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_TRUE(
      ValidateGainState(fha::GainState{{
                            // muted (optional) is missing: NOT muted
                            .agc_enabled = false,
                            .gain_db = -12.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = false,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_TRUE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            // agc_enabled (optional) is missing: NOT enabled
                            .gain_db = -12.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = false,
                            .min_gain_db = -24.0f,
                            .max_gain_db = -12.0f,
                            .gain_step_db = 2.0f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
}

TEST(ValidateTest, ValidatePlugState) {
  EXPECT_TRUE(ValidatePlugState(fha::PlugState{{
      .plugged = true,
      .plug_state_time = 0,
  }}));
  EXPECT_TRUE(ValidatePlugState(fha::PlugState{{
      .plugged = false,
      .plug_state_time = zx::clock::get_monotonic().get(),
  }}));
  EXPECT_TRUE(ValidatePlugState(fha::PlugState{{
                                    .plugged = true,
                                    .plug_state_time = zx::clock::get_monotonic().get(),
                                }},
                                fha::PlugDetectCapabilities::kHardwired));
  EXPECT_TRUE(ValidatePlugState(fha::PlugState{{
                                    .plugged = false,
                                    .plug_state_time = zx::clock::get_monotonic().get(),
                                }},
                                fha::PlugDetectCapabilities::kCanAsyncNotify));
}

TEST(ValidateTest, ValidateRingBufferFormatSets) {
  EXPECT_TRUE(ValidateRingBufferFormatSets({CompliantFormatSet()}));

  fha::SupportedFormats minimal_values_format_set;
  minimal_values_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {{.max_frequency = 50}},
            }}}},
      }},
      .sample_formats = {{fha::SampleFormat::kPcmUnsigned}},
      .bytes_per_sample = {{1}},
      .valid_bits_per_sample = {{1}},
      .frame_rates = {{1000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({minimal_values_format_set}));

  fha::SupportedFormats maximal_values_format_set;
  maximal_values_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {},
                {{.max_frequency = 96000}},
            }}}},
      }},
      .sample_formats = {{fha::SampleFormat::kPcmFloat}},
      .bytes_per_sample = {{8}},
      .valid_bits_per_sample = {{64}},
      .frame_rates = {{192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({maximal_values_format_set}));

  // Probe fully-populated values, in multiple format sets.
  fha::SupportedFormats signed_format_set;
  signed_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 1000}},
                {{.max_frequency = 2500}},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 0, .max_frequency = 24000}},
                {{.min_frequency = 16000, .max_frequency = 24000}},
                {{.min_frequency = 0, .max_frequency = 96000}},
                {{.min_frequency = 16000, .max_frequency = 96000}},
            }}}},
          {{.attributes = {{
                {{.max_frequency = 2500}},
                {},
                {{.min_frequency = 24000}},
            }}}},
      }},
      .sample_formats = {{fha::SampleFormat::kPcmSigned}},
      .bytes_per_sample = {{2, 4}},
      .valid_bits_per_sample = {{1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11, 12,
                                 13, 14, 15, 16, 18, 20, 22, 24, 26, 28, 30, 32}},
      .frame_rates = {{1000, 2000, 4000, 8000, 11025, 16000, 22050, 24000, 44100, 48000, 88200,
                       96000, 192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({signed_format_set}));

  fha::SupportedFormats unsigned_format_set;
  unsigned_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 1000}},
                {{.max_frequency = 2500}},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 0, .max_frequency = 24000}},
                {{.min_frequency = 16000, .max_frequency = 24000}},
                {{.min_frequency = 0, .max_frequency = 96000}},
                {{.min_frequency = 16000, .max_frequency = 96000}},
            }}}},
          {{.attributes = {{
                {{.max_frequency = 2500}},
                {},
                {{.min_frequency = 24000}},
            }}}},
      }},
      .sample_formats = {{fha::SampleFormat::kPcmUnsigned}},
      .bytes_per_sample = {{1}},
      .valid_bits_per_sample = {{1, 2, 3, 4, 5, 6, 7, 8}},
      .frame_rates = {{1000, 2000, 4000, 8000, 11025, 16000, 22050, 24000, 44100, 48000, 88200,
                       96000, 192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({unsigned_format_set}));

  fha::SupportedFormats float_format_set;
  float_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 1000}},
                {{.max_frequency = 2500}},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 0, .max_frequency = 24000}},
                {{.min_frequency = 2500, .max_frequency = 24000}},
                {{.min_frequency = 16000, .max_frequency = 96000}},
                {{.min_frequency = 2400, .max_frequency = 96000}},
            }}}},
          {{.attributes = {{
                {{.min_frequency = 1000}},
                {},
                {{.max_frequency = 2500}},
            }}}},
      }},
      .sample_formats = {{fha::SampleFormat::kPcmFloat}},
      .bytes_per_sample = {{4, 8}},
      .valid_bits_per_sample = {{1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11, 12, 13, 14, 15, 16,
                                 18, 20, 22, 24, 26, 28, 30, 32, 36, 40, 44, 48, 52, 56, 60, 64}},
      .frame_rates = {{1000, 2000, 4000, 8000, 11025, 16000, 22050, 24000, 44100, 48000, 88200,
                       96000, 192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({float_format_set}));

  std::vector supported_formats = {CompliantFormatSet()};
  supported_formats.push_back(signed_format_set);
  supported_formats.push_back(unsigned_format_set);
  supported_formats.push_back(float_format_set);
  supported_formats.push_back(minimal_values_format_set);
  supported_formats.push_back(maximal_values_format_set);
  EXPECT_TRUE(ValidateRingBufferFormatSets(supported_formats));
}

// TODO(https://fxbug.dev/42069012): Unittest TranslateRingBufferFormatSets

TEST(ValidateTest, ValidateRingBufferFormat) {
  for (auto chans : kChannels) {
    for (auto [bytes, sample_format] : kFormats) {
      for (auto rate : kFrameRates) {
        EXPECT_TRUE(ValidateRingBufferFormat(fha::Format{{
            .pcm_format = fha::PcmFormat{{
                .number_of_channels = chans,
                .sample_format = sample_format,
                .bytes_per_sample = bytes,
                .valid_bits_per_sample = 1,
                .frame_rate = rate,
            }},
        }}));
        EXPECT_TRUE(ValidateRingBufferFormat(fha::Format{{
            .pcm_format = fha::PcmFormat{{
                .number_of_channels = chans,
                .sample_format = sample_format,
                .bytes_per_sample = bytes,
                .valid_bits_per_sample = static_cast<uint8_t>(bytes * 8 - 4),
                .frame_rate = rate,
            }},
        }}));
        EXPECT_TRUE(ValidateRingBufferFormat(fha::Format{{
            .pcm_format = fha::PcmFormat{{
                .number_of_channels = chans,
                .sample_format = sample_format,
                .bytes_per_sample = bytes,
                .valid_bits_per_sample = static_cast<uint8_t>(bytes * 8),
                .frame_rate = rate,
            }},
        }}));
      }
    }
  }
}

TEST(ValidateTest, ValidateSampleFormatCompatibility) {
  for (auto [bytes, sample_format] : kFormats) {
    EXPECT_TRUE(ValidateSampleFormatCompatibility(bytes, sample_format));
  }
}

TEST(ValidateTest, ValidateRingBufferVmo) {
  constexpr uint64_t kVmoContentSize = 4096;
  zx::vmo vmo;
  auto status = zx::vmo::create(kVmoContentSize, 0, &vmo);
  ASSERT_EQ(status, ZX_OK) << "could not create VMO for test input";

  constexpr uint8_t kChannelCount = 1;
  constexpr uint8_t kSampleSize = 2;
  uint32_t num_frames = static_cast<uint32_t>(kVmoContentSize / kChannelCount / kSampleSize);
  EXPECT_TRUE(ValidateRingBufferVmo(vmo, num_frames,
                                    {{
                                        .pcm_format = fha::PcmFormat{{
                                            .number_of_channels = kChannelCount,
                                            .sample_format = fha::SampleFormat::kPcmSigned,
                                            .bytes_per_sample = kSampleSize,
                                            .valid_bits_per_sample = 16,
                                            .frame_rate = 8000,
                                        }},
                                    }}));
}

TEST(ValidateTest, ValidateDelayInfo) {
  EXPECT_TRUE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = 0,
  }}));
  EXPECT_TRUE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = 125,
      .external_delay = 0,
  }}));
  EXPECT_TRUE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = 0,
      .external_delay = 125,
  }}));
}

TEST(ValidateTest, ValidateDaiFormatSets) {
  EXPECT_TRUE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
}

TEST(ValidateTest, ValidateDaiFormat) {
  // Normal values
  EXPECT_TRUE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));
  // Minimal values
  EXPECT_TRUE(ValidateDaiFormat({{
      .number_of_channels = 1,
      .channels_to_use_bitmask = 0x01,
      .sample_format = fha::DaiSampleFormat::kPdm,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kNone),
      .frame_rate = 1000,
      .bits_per_slot = 8,
      .bits_per_sample = 1,
  }}));
  // Maximal values
  EXPECT_TRUE(ValidateDaiFormat({{
      .number_of_channels = 32,
      .channels_to_use_bitmask = 0xFFFFFFFF,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format = fha::DaiFrameFormat::WithFrameFormatCustom({{
          .left_justified = true,
          .sclk_on_raising = false,
          .frame_sync_sclks_offset = 0,
          .frame_sync_size = 1,
      }}),
      .frame_rate = 192000,
      .bits_per_slot = 32,
      .bits_per_sample = 32,
  }}));
  // Maximal values
  EXPECT_TRUE(ValidateDaiFormat({{
      .number_of_channels = 64,
      .channels_to_use_bitmask = 0xFFFFFFFFFFFFFFFF,
      .sample_format = fha::DaiSampleFormat::kPcmFloat,
      .frame_format = fha::DaiFrameFormat::WithFrameFormatCustom({{
          .left_justified = true,
          .sclk_on_raising = false,
          .frame_sync_sclks_offset = -128,
          .frame_sync_size = 255,
      }}),
      .frame_rate = 192000 * 64 * 8,
      .bits_per_slot = kMaxSupportedDaiFormatBitsPerSlot,
      .bits_per_sample = kMaxSupportedDaiFormatBitsPerSlot,
  }}));
}

TEST(ValidateTest, ValidateCodecFormatInfo) {
  EXPECT_TRUE(ValidateCodecFormatInfo(fha::CodecFormatInfo{}));
  // For all three fields, test missing, minimal and maximal values.
  EXPECT_TRUE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .external_delay = 0,
      .turn_off_delay = zx::time::infinite().get(),
  }}));
  EXPECT_TRUE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .external_delay = zx::time::infinite().get(),
      .turn_on_delay = 0,
  }}));
  EXPECT_TRUE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_on_delay = zx::time::infinite().get(),
      .turn_off_delay = 0,
  }}));
}

// signalprocessing functions
TEST(ValidateTest, ValidateTopologies) {
  EXPECT_TRUE(ValidateTopologies(kTopologies, MapElements(kElements)));
}

TEST(ValidateTest, ValidateTopology) {
  EXPECT_TRUE(ValidateTopology(kTopologyDaiAgcDynRb, MapElements(kElements)));
  EXPECT_TRUE(ValidateTopology(kTopologyDaiRb, MapElements(kElements)));
  EXPECT_TRUE(ValidateTopology(kTopologyRbDai, MapElements(kElements)));
}

TEST(ValidateTest, ValidateElements) { EXPECT_TRUE(ValidateElements(kElements)); }

TEST(ValidateTest, ValidateElement) {
  EXPECT_TRUE(ValidateElement(kAgcElement));
  EXPECT_TRUE(ValidateElement(kRingBufferElement));
}

TEST(ValidateTest, ValidateDynamicsElement) {
  EXPECT_TRUE(ValidateDynamicsElement(kDynamicsElement));

  EXPECT_TRUE(ValidateElement(kDynamicsElement));
}

TEST(ValidateTest, ValidateDaiEndpointElement) {
  EXPECT_TRUE(ValidateEndpointElement(kDaiEndpointElement));

  EXPECT_TRUE(ValidateElement(kDaiEndpointElement));
}

TEST(ValidateTest, ValidateEqualizerElement) {
  EXPECT_TRUE(ValidateEqualizerElement(kEqualizerElement));

  EXPECT_TRUE(ValidateElement(kEqualizerElement));
}

TEST(ValidateTest, ValidateGainElement) {
  EXPECT_TRUE(ValidateGainElement(kGainElement));

  EXPECT_TRUE(ValidateElement(kGainElement));
}

TEST(ValidateTest, ValidateVendorSpecificElement) {
  EXPECT_TRUE(ValidateVendorSpecificElement(kVendorSpecificElement));

  EXPECT_TRUE(ValidateElement(kVendorSpecificElement));
}

// ValidateElementState
TEST(ValidateTest, ValidateElementState) {
  EXPECT_TRUE(ValidateElementState(kGenericElementState, kAgcElement));
}

TEST(ValidateTest, ValidateDynamicsElementState) {
  EXPECT_TRUE(ValidateDynamicsElementState(kDynamicsElementState, kDynamicsElement));

  EXPECT_TRUE(ValidateElementState(kDynamicsElementState, kDynamicsElement));
}

TEST(ValidateTest, ValidateEndpointElementState) {
  EXPECT_TRUE(ValidateEndpointElementState(kEndpointElementState, kDaiEndpointElement));

  EXPECT_TRUE(ValidateElementState(kEndpointElementState, kDaiEndpointElement));
}

TEST(ValidateTest, ValidateEqualizerElementState) {
  EXPECT_TRUE(ValidateEqualizerElementState(kEqualizerElementState, kEqualizerElement));

  EXPECT_TRUE(ValidateElementState(kEqualizerElementState, kEqualizerElement));
}

TEST(ValidateTest, ValidateGainElementState) {
  EXPECT_TRUE(ValidateGainElementState(kGainElementState, kGainElement));

  EXPECT_TRUE(ValidateElementState(kGainElementState, kGainElement));
}

TEST(ValidateTest, ValidateVendorSpecificElementState) {
  EXPECT_TRUE(
      ValidateVendorSpecificElementState(kVendorSpecificElementState, kVendorSpecificElement));

  EXPECT_TRUE(ValidateElementState(kVendorSpecificElementState, kVendorSpecificElement));
}

}  // namespace
}  // namespace media_audio

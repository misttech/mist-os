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

// These cases unittest the Validate... functions with inputs that cause INFO logging (if any).

const std::vector<uint8_t> kChannels = {1, 8, 255};
const std::vector<std::pair<uint8_t, fuchsia_hardware_audio::SampleFormat>> kFormats = {
    {1, fuchsia_hardware_audio::SampleFormat::kPcmUnsigned},
    {2, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
    {4, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
    {4, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
    {8, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
};
const std::vector<uint32_t> kFrameRates = {1000, 44100, 48000, 19200};

// Unittest ValidateCodecProperties -- the missing, minimal and maximal possibilities
TEST(ValidateTest, ValidateCodecProperties) {
  EXPECT_TRUE(ValidateCodecProperties(fuchsia_hardware_audio::CodecProperties{{
      // is_input missing
      .manufacturer = " ",  // minimal value (empty is disallowed)
      .product =            // maximal value
      "Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name extends to 321X",
      // unique_id missing
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
  }}));
  EXPECT_TRUE(ValidateCodecProperties(fuchsia_hardware_audio::CodecProperties{{
      .is_input = false,
      .manufacturer =  // maximal value
      "Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long, which extends to... 321X",
      // product missing
      .unique_id = {{}},  // minimal value
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify,
  }}));
  EXPECT_TRUE(ValidateCodecProperties(fuchsia_hardware_audio::CodecProperties{{
      .is_input = true,
      // manufacturer missing
      .product = " ",  // minimal value (empty is disallowed)
      .unique_id = {{0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  //
                     0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF}},
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify,
  }}));
}

// Unittest ValidateCompositeProperties -- the missing, minimal and maximal possibilities
TEST(ValidateTest, ValidateCompositeProperties) {
  EXPECT_TRUE(ValidateCompositeProperties(fuchsia_hardware_audio::CompositeProperties{{
      // manufacturer missing
      .product = " ",  // minimal value (empty is disallowed)
      .unique_id = {{0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,  //
                     0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF}},
      .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
  }}));
  EXPECT_TRUE(ValidateCompositeProperties(fuchsia_hardware_audio::CompositeProperties{{
      .manufacturer = " ",  // minimal value (empty is disallowed)
      .product =            // maximal value
      "Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name is 256 characters long; Maximum allowed Product name extends to 321X",
      // unique_id missing
      .clock_domain = fuchsia_hardware_audio::kClockDomainExternal,
  }}));
  EXPECT_TRUE(ValidateCompositeProperties(fuchsia_hardware_audio::CompositeProperties{{
      .manufacturer =  // maximal value
      "Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long; Maximum allowed Manufacturer name is 256 characters long, which extends to... 321X",
      // product missing
      .unique_id = {{}},  // minimal value
      .clock_domain = fuchsia_hardware_audio::kClockDomainExternal,
  }}));
}

TEST(ValidateTest, ValidateStreamProperties) {
  fuchsia_hardware_audio::StreamProperties stream_properties{{
      .is_input = false,
      .min_gain_db = 0.0f,
      .max_gain_db = 0.0f,
      .gain_step_db = 0.0f,
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
      .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
  }};
  fuchsia_hardware_audio::GainState gain_state{{.gain_db = 0.0f}};
  fuchsia_hardware_audio::PlugState plug_state{{.plugged = true, .plug_state_time = 0}};

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
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify,
      .manufacturer = "Fuchsia2",
      .product = "Test2",
      .clock_domain = fuchsia_hardware_audio::kClockDomainExternal,
  }};
  gain_state = {{.muted = true, .agc_enabled = true, .gain_db = -4.2f}};
  plug_state = {{.plugged = false, .plug_state_time = zx::clock::get_monotonic().get()}};

  EXPECT_TRUE(ValidateStreamProperties(stream_properties));
  EXPECT_TRUE(ValidateStreamProperties(stream_properties, gain_state));
  EXPECT_TRUE(ValidateStreamProperties(stream_properties, gain_state, plug_state));
}

// TODO(https://fxbug.dev/42069012): Unittest ValidateDeviceInfo, incl. manuf & prod 256 chars long

fuchsia_hardware_audio::SupportedFormats CompliantFormatSet() {
  return fuchsia_hardware_audio::SupportedFormats{{
      .pcm_supported_formats = fuchsia_hardware_audio::PcmSupportedFormats{{
          .channel_sets = {{
              fuchsia_hardware_audio::ChannelSet{{
                  .attributes = {{
                      fuchsia_hardware_audio::ChannelAttributes{{
                          .min_frequency = 20,
                          .max_frequency = 20000,
                      }},
                  }},
              }},
          }},
          .sample_formats = {{fuchsia_hardware_audio::SampleFormat::kPcmSigned}},
          .bytes_per_sample = {{2}},
          .valid_bits_per_sample = {{16}},
          .frame_rates = {{48000}},
      }},
  }};
}

TEST(ValidateTest, ValidateRingBufferProperties) {
  EXPECT_TRUE(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = false,
      .turn_on_delay = 125,
      .driver_transfer_bytes = 32,
  }}));

  // TODO(b/311694769): Resolve driver_transfer_bytes lower limit: specifically is 0 allowed?
  EXPECT_TRUE(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      .turn_on_delay = 0,  // can be zero
      .driver_transfer_bytes = 1,
  }}));

  // TODO(b/311694769): Resolve driver_transfer_bytes upper limit: no limit? Soft guideline?
  EXPECT_TRUE(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      // turn_on_delay (optional) is missing
      .driver_transfer_bytes = 128,
  }}));
}

TEST(ValidateTest, ValidateGainState) {
  EXPECT_TRUE(ValidateGainState(fuchsia_hardware_audio::GainState{{
      // muted (optional) is missing: NOT muted
      // agc_enabled (optional) is missing: NOT enabled
      .gain_db = -42.0f,
  }}  // If StreamProperties is not provided, assume the device cannot mute or agc
                                ));
  EXPECT_TRUE(
      ValidateGainState(fuchsia_hardware_audio::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = 0,
                        }},
                        // If StreamProperties is not provided, assume the device cannot mute or agc
                        std::nullopt));
  EXPECT_TRUE(ValidateGainState(
      fuchsia_hardware_audio::GainState{{
          .muted = false,
          .agc_enabled = true,
          .gain_db = -12.0f,
      }},
      fuchsia_hardware_audio::StreamProperties{{
          .is_input = false,
          // can_mute (optional) is missing: device cannot mute
          .can_agc = true,
          .min_gain_db = -12.0f,
          .max_gain_db = 12.0f,
          .gain_step_db = 0.5f,
          .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
          .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
      }}));
  EXPECT_TRUE(ValidateGainState(
      fuchsia_hardware_audio::GainState{{
          .muted = true,
          .agc_enabled = false,
          .gain_db = -12.0f,
      }},
      fuchsia_hardware_audio::StreamProperties{{
          .is_input = false,
          .can_mute = true,
          // can_agc (optional) is missing: device cannot agc
          .min_gain_db = -24.0f,
          .max_gain_db = -12.0f,
          .gain_step_db = 2.0f,
          .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
          .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
      }}));
  EXPECT_TRUE(ValidateGainState(
      fuchsia_hardware_audio::GainState{{
          // muted (optional) is missing: NOT muted
          .agc_enabled = false,
          .gain_db = -12.0f,
      }},
      fuchsia_hardware_audio::StreamProperties{{
          .is_input = false,
          .can_mute = false,
          .can_agc = true,
          .min_gain_db = -12.0f,
          .max_gain_db = 12.0f,
          .gain_step_db = 0.5f,
          .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
          .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
      }}));
  EXPECT_TRUE(ValidateGainState(
      fuchsia_hardware_audio::GainState{{
          .muted = false,
          // agc_enabled (optional) is missing: NOT enabled
          .gain_db = -12.0f,
      }},
      fuchsia_hardware_audio::StreamProperties{{
          .is_input = false,
          .can_mute = true,
          .can_agc = false,
          .min_gain_db = -24.0f,
          .max_gain_db = -12.0f,
          .gain_step_db = 2.0f,
          .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
          .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
      }}));
}

TEST(ValidateTest, ValidatePlugState) {
  EXPECT_TRUE(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
      .plugged = true,
      .plug_state_time = 0,
  }}));
  EXPECT_TRUE(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
      .plugged = false,
      .plug_state_time = zx::clock::get_monotonic().get(),
  }}));
  EXPECT_TRUE(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                    .plugged = true,
                                    .plug_state_time = zx::clock::get_monotonic().get(),
                                }},
                                fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired));
  EXPECT_TRUE(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                    .plugged = false,
                                    .plug_state_time = zx::clock::get_monotonic().get(),
                                }},
                                fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify));
}

TEST(ValidateTest, ValidateRingBufferFormatSets) {
  EXPECT_TRUE(ValidateRingBufferFormatSets({CompliantFormatSet()}));

  fuchsia_hardware_audio::SupportedFormats minimal_values_format_set;
  minimal_values_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {{.max_frequency = 50}},
            }}}},
      }},
      .sample_formats = {{fuchsia_hardware_audio::SampleFormat::kPcmUnsigned}},
      .bytes_per_sample = {{1}},
      .valid_bits_per_sample = {{1}},
      .frame_rates = {{1000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({minimal_values_format_set}));

  fuchsia_hardware_audio::SupportedFormats maximal_values_format_set;
  maximal_values_format_set.pcm_supported_formats() = {{
      .channel_sets = {{
          {{.attributes = {{
                {},
                {{.max_frequency = 96000}},
            }}}},
      }},
      .sample_formats = {{fuchsia_hardware_audio::SampleFormat::kPcmFloat}},
      .bytes_per_sample = {{8}},
      .valid_bits_per_sample = {{64}},
      .frame_rates = {{192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({maximal_values_format_set}));

  // Probe fully-populated values, in multiple format sets.
  fuchsia_hardware_audio::SupportedFormats signed_format_set;
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
      .sample_formats = {{fuchsia_hardware_audio::SampleFormat::kPcmSigned}},
      .bytes_per_sample = {{2, 4}},
      .valid_bits_per_sample = {{1,  2,  3,  4,  5,  6,  7,  8,  9,  10, 11, 12,
                                 13, 14, 15, 16, 18, 20, 22, 24, 26, 28, 30, 32}},
      .frame_rates = {{1000, 2000, 4000, 8000, 11025, 16000, 22050, 24000, 44100, 48000, 88200,
                       96000, 192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({signed_format_set}));

  fuchsia_hardware_audio::SupportedFormats unsigned_format_set;
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
      .sample_formats = {{fuchsia_hardware_audio::SampleFormat::kPcmUnsigned}},
      .bytes_per_sample = {{1}},
      .valid_bits_per_sample = {{1, 2, 3, 4, 5, 6, 7, 8}},
      .frame_rates = {{1000, 2000, 4000, 8000, 11025, 16000, 22050, 24000, 44100, 48000, 88200,
                       96000, 192000}},
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets({unsigned_format_set}));

  fuchsia_hardware_audio::SupportedFormats float_format_set;
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
      .sample_formats = {{fuchsia_hardware_audio::SampleFormat::kPcmFloat}},
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
        EXPECT_TRUE(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
            .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                .number_of_channels = chans,
                .sample_format = sample_format,
                .bytes_per_sample = bytes,
                .valid_bits_per_sample = 1,
                .frame_rate = rate,
            }},
        }}));
        EXPECT_TRUE(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
            .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                .number_of_channels = chans,
                .sample_format = sample_format,
                .bytes_per_sample = bytes,
                .valid_bits_per_sample = static_cast<uint8_t>(bytes * 8 - 4),
                .frame_rate = rate,
            }},
        }}));
        EXPECT_TRUE(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
            .pcm_format = fuchsia_hardware_audio::PcmFormat{{
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
  EXPECT_TRUE(ValidateRingBufferVmo(
      vmo, num_frames,
      {{
          .pcm_format = fuchsia_hardware_audio::PcmFormat{{
              .number_of_channels = kChannelCount,
              .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
              .bytes_per_sample = kSampleSize,
              .valid_bits_per_sample = 16,
              .frame_rate = 8000,
          }},
      }}));
}

TEST(ValidateTest, ValidateDelayInfo) {
  EXPECT_TRUE(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
      .internal_delay = 0,
  }}));
  EXPECT_TRUE(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
      .internal_delay = 125,
      .external_delay = 0,
  }}));
  EXPECT_TRUE(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
      .internal_delay = 0,
      .external_delay = 125,
  }}));
}

TEST(ValidateTest, ValidateDaiFormatSets) {
  EXPECT_TRUE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
              fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
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
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));
  // Minimal values
  EXPECT_TRUE(ValidateDaiFormat({{
      .number_of_channels = 1,
      .channels_to_use_bitmask = 0x01,
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPdm,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kNone),
      .frame_rate = 1000,
      .bits_per_slot = 8,
      .bits_per_sample = 1,
  }}));
  // Maximal values
  EXPECT_TRUE(ValidateDaiFormat({{
      .number_of_channels = 32,
      .channels_to_use_bitmask = 0xFFFFFFFF,
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatCustom({{
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
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmFloat,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatCustom({{
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
  EXPECT_TRUE(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{}));
  // For all three fields, test missing, minimal and maximal values.
  EXPECT_TRUE(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
      .external_delay = 0,
      .turn_off_delay = zx::time::infinite().get(),
  }}));
  EXPECT_TRUE(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
      .external_delay = zx::time::infinite().get(),
      .turn_on_delay = 0,
  }}));
  EXPECT_TRUE(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
      .turn_on_delay = zx::time::infinite().get(),
      .turn_off_delay = 0,
  }}));
}

// signalprocessing functions
TEST(ValidateTest, ValidateElements) { EXPECT_TRUE(ValidateElements(kElements)); }

TEST(ValidateTest, ValidateElement) {
  EXPECT_TRUE(ValidateElement(kElement1));
  EXPECT_TRUE(ValidateElement(kElement2));
  EXPECT_TRUE(ValidateElement(kElement3));
  EXPECT_TRUE(ValidateElement(kElement4));
}

TEST(ValidateTest, MapElements) {
  auto map = MapElements(kElements);
  EXPECT_EQ(map.size(), kElements.size());

  EXPECT_EQ(*map.at(*kElement1.id()).element.type(), *kElement1.type());
  EXPECT_EQ(*map.at(*kElement1.id()).element.type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect);

  EXPECT_EQ(*map.at(*kElement2.id()).element.type(), *kElement2.type());

  EXPECT_EQ(*map.at(*kElement3.id()).element.type(), *kElement3.type());
  EXPECT_TRUE(map.at(*kElement3.id()).element.can_disable().value_or(false));
  EXPECT_EQ(map.at(*kElement3.id()).element.description()->at(255), 'X');

  EXPECT_EQ(*map.at(*kElement4.id()).element.type(), *kElement4.type());
  EXPECT_EQ(*map.at(*kElement4.id()).element.type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer);
}

TEST(ValidateTest, ValidateTopologies) {
  EXPECT_TRUE(ValidateTopologies(kTopologies, MapElements(kElements)));
}

TEST(ValidateTest, ValidateTopology) {
  EXPECT_TRUE(ValidateTopology(kTopology1234, MapElements(kElements)));
  EXPECT_TRUE(ValidateTopology(kTopology14, MapElements(kElements)));
  EXPECT_TRUE(ValidateTopology(kTopology41, MapElements(kElements)));
}

TEST(ValidateTest, MapTopologies) {
  auto map = MapTopologies(kTopologies);
  EXPECT_EQ(map.size(), 3u);

  EXPECT_EQ(map.at(kTopologyId1234).size(), 3u);
  EXPECT_EQ(map.at(kTopologyId1234).at(0).processing_element_id_from(), kElementId1);
  EXPECT_EQ(map.at(kTopologyId1234).at(0).processing_element_id_to(), kElementId2);

  EXPECT_EQ(map.at(kTopologyId14).size(), 1u);
  EXPECT_EQ(map.at(kTopologyId14).front().processing_element_id_from(), kElementId1);
  EXPECT_EQ(map.at(kTopologyId14).front().processing_element_id_to(), kElementId4);

  EXPECT_EQ(map.at(kTopologyId41).size(), 1u);
  EXPECT_EQ(map.at(kTopologyId41).front().processing_element_id_from(), kElementId4);
  EXPECT_EQ(map.at(kTopologyId41).front().processing_element_id_to(), kElementId1);
}

// ValidateElementState
TEST(ValidateTest, ValidateElementState) {
  EXPECT_TRUE(ValidateElementState(kElementState1, kElement1));

  // Add more cases here
}

}  // namespace media_audio

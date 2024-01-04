// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/validate.h"

#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>

#include <optional>
#include <vector>

#include <gtest/gtest.h>

// These cases unittest the Validate... functions with inputs that cause INFO logging (if any).

namespace media_audio {

const std::vector<uint8_t> kChannels = {1, 8, 255};
const std::vector<std::pair<uint8_t, fuchsia_hardware_audio::SampleFormat>> kFormats = {
    {1, fuchsia_hardware_audio::SampleFormat::kPcmUnsigned},
    {2, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
    {4, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
    {4, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
    {8, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
};
const std::vector<uint32_t> kFrameRates = {1000, 44100, 48000, 19200};

// TODO(https://fxbug.dev/117826): Unittest TranslateFormatSets
// TEST(ValidateTest, TranslateFormatSets) {}

// Unittest ValidateStreamProperties
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

  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_OK);
  EXPECT_EQ(ValidateStreamProperties(stream_properties, gain_state), ZX_OK);
  EXPECT_EQ(ValidateStreamProperties(stream_properties, gain_state, plug_state), ZX_OK);

  stream_properties = {{
      .unique_id = {{255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
                     255}},
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

  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_OK);
  EXPECT_EQ(ValidateStreamProperties(stream_properties, gain_state), ZX_OK);
  EXPECT_EQ(ValidateStreamProperties(stream_properties, gain_state, plug_state), ZX_OK);
}

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

// Unittest ValidateSupportedFormats
TEST(ValidateTest, ValidateSupportedFormats) {
  EXPECT_EQ(ValidateSupportedFormats({CompliantFormatSet()}), ZX_OK);

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
  EXPECT_EQ(ValidateSupportedFormats({minimal_values_format_set}), ZX_OK);

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
  EXPECT_EQ(ValidateSupportedFormats({maximal_values_format_set}), ZX_OK);

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
  EXPECT_EQ(ValidateSupportedFormats({signed_format_set}), ZX_OK);

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
  EXPECT_EQ(ValidateSupportedFormats({unsigned_format_set}), ZX_OK);

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
  EXPECT_EQ(ValidateSupportedFormats({float_format_set}), ZX_OK);

  std::vector supported_formats = {CompliantFormatSet()};
  supported_formats.push_back(signed_format_set);
  supported_formats.push_back(unsigned_format_set);
  supported_formats.push_back(float_format_set);
  supported_formats.push_back(minimal_values_format_set);
  supported_formats.push_back(maximal_values_format_set);
  EXPECT_EQ(ValidateSupportedFormats(supported_formats), ZX_OK);
}

// Unittest ValidateGainState
TEST(ValidateTest, ValidateGainState) {
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                // muted (optional) is missing: NOT muted
                // agc_enabled (optional) is missing: NOT enabled
                .gain_db = -42.0f,
            }}),
            // If StreamProperties is not provided, assume the device cannot mute or agc
            ZX_OK);
  EXPECT_EQ(
      ValidateGainState(
          fuchsia_hardware_audio::GainState{{
              .muted = false,
              .agc_enabled = false,
              .gain_db = 0,
          }},
          std::nullopt  // If StreamProperties is not provided, assume the device cannot mute or agc
          ),
      ZX_OK);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
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
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_OK);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
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
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_OK);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
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
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_OK);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
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
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_OK);
}

// Unittest ValidatePlugState
TEST(ValidateTest, ValidatePlugState) {
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                .plugged = true,
                .plug_state_time = 0,
            }}),
            ZX_OK);
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                .plugged = false,
                .plug_state_time = zx::clock::get_monotonic().get(),
            }}),
            ZX_OK);
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                  .plugged = true,
                                  .plug_state_time = zx::clock::get_monotonic().get(),
                              }},
                              fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired),
            ZX_OK);
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                  .plugged = false,
                                  .plug_state_time = zx::clock::get_monotonic().get(),
                              }},
                              fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify),
            ZX_OK);
}

// TODO(https://fxbug.dev/117826): Unittest ValidateDeviceInfo
// TODO(https://fxbug.dev/117826): manufacturer and product strings that are 256 chars long
// TEST(ValidateTest, ValidateDeviceInfo) {}

// Unittest ValidateRingBufferProperties
TEST(ValidateTest, ValidateRingBufferProperties) {
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = false,
                .turn_on_delay = 125,
                .driver_transfer_bytes = 32,
            }}),
            ZX_OK);

  // TODO(b/311694769): Resolve driver_transfer_bytes lower limit: specifically is 0 allowed?
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = 0,  // can be zero
                .driver_transfer_bytes = 1,
            }}),
            ZX_OK);

  // TODO(b/311694769): Resolve driver_transfer_bytes upper limit: no limit? Soft guideline?
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = true,
                // turn_on_delay (optional) is missing
                .driver_transfer_bytes = 128,
            }}),
            ZX_OK);
}

// Unittest ValidateRingBufferFormat
TEST(ValidateTest, ValidateRingBufferFormat) {
  for (auto chans : kChannels) {
    for (auto [bytes, sample_format] : kFormats) {
      for (auto rate : kFrameRates) {
        EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                      .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                          .number_of_channels = chans,
                          .sample_format = sample_format,
                          .bytes_per_sample = bytes,
                          .valid_bits_per_sample = 1,
                          .frame_rate = rate,
                      }},
                  }}),
                  ZX_OK);
        EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                      .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                          .number_of_channels = chans,
                          .sample_format = sample_format,
                          .bytes_per_sample = bytes,
                          .valid_bits_per_sample = static_cast<uint8_t>(bytes * 8 - 4),
                          .frame_rate = rate,
                      }},
                  }}),
                  ZX_OK);
        EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                      .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                          .number_of_channels = chans,
                          .sample_format = sample_format,
                          .bytes_per_sample = bytes,
                          .valid_bits_per_sample = static_cast<uint8_t>(bytes * 8),
                          .frame_rate = rate,
                      }},
                  }}),
                  ZX_OK);
      }
    }
  }
}

// Unittest ValidateFormatCompatibility
TEST(ValidateTest, ValidateFormatCompatibility) {
  for (auto [bytes, sample_format] : kFormats) {
    EXPECT_EQ(ValidateFormatCompatibility(bytes, sample_format), ZX_OK);
  }
}

// Unittest ValidateRingBufferVmo
TEST(ValidateTest, ValidateRingBufferVmo) {
  constexpr uint64_t kVmoContentSize = 4096;
  zx::vmo vmo;
  auto status = zx::vmo::create(kVmoContentSize, 0, &vmo);
  ASSERT_EQ(status, ZX_OK) << "could not create VMO for test input";

  constexpr uint8_t kChannelCount = 1;
  constexpr uint8_t kSampleSize = 2;
  uint32_t num_frames = static_cast<uint32_t>(kVmoContentSize / kChannelCount / kSampleSize);
  EXPECT_EQ(ValidateRingBufferVmo(
                vmo, num_frames,
                {{
                    .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                        .number_of_channels = kChannelCount,
                        .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                        .bytes_per_sample = kSampleSize,
                        .valid_bits_per_sample = 16,
                        .frame_rate = 8000,
                    }},
                }}),
            ZX_OK);
}

// Unittest ValidateDelayInfo
TEST(ValidateTest, ValidateDelayInfo) {
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                .internal_delay = 0,
            }}),
            ZX_OK);
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                .internal_delay = 125,
                .external_delay = 0,
            }}),
            ZX_OK);
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                .internal_delay = 0,
                .external_delay = 125,
            }}),
            ZX_OK);
}

}  // namespace media_audio

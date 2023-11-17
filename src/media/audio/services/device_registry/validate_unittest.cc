// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/validate.h"

#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <optional>
#include <vector>

#include <gtest/gtest.h>

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

// TODO(fxbug.dev/117826): Unittest TranslateFormatSets
// TEST(ValidateTest, TranslateFormatSets) {}

// TODO(fxbug.dev/117826): Unittest ValidateStreamProperties
// TEST(ValidateTest, ValidateStreamProperties) {}

// TODO(fxbug.dev/117826): Unittest ValidateSupportedFormats
// TEST(ValidateTest, ValidateSupportedFormats) {}

// Unittest ValidateGainState
TEST(ValidateTest, ValidateGainState) {
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                // muted is missing: NOT muted
                // agc_enabled is missing: NOT enabled
                .gain_db = -42.0f,
            }}  // StreamProperties is missing: device has no mute or agc
                              ),
            ZX_OK);
  EXPECT_EQ(
      ValidateGainState(fuchsia_hardware_audio::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = 0,
                        }},
                        std::nullopt  // StreamProperties is missing: device has no mute or agc
                        ),
      ZX_OK);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = true,
                                  .gain_db = -12.0f,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  // can_mute is missing
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
                                  // can_agc is missing
                                  .min_gain_db = -24.0f,
                                  .max_gain_db = -12.0f,
                                  .gain_step_db = 2.0f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_OK);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  // muted is missing: NOT muted
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
                                  // agc_enabled is missing: NOT enabled
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

// TODO(fxbug.dev/117826): Unittest ValidateDeviceInfo
// TODO(fxbug.dev/117826): manufacturer and product strings that are 256 chars long
// TEST(ValidateTest, ValidateDeviceInfo) {}

// Unittest ValidateRingBufferProperties
TEST(ValidateTest, ValidateRingBufferProperties) {
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = true,
                // turn_on_delay is missing
                .driver_transfer_bytes = 128,
            }}),
            ZX_OK);

  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = false,
                .turn_on_delay = 125,
                .driver_transfer_bytes = 32,
            }}),
            ZX_OK);

  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = 0,
                .driver_transfer_bytes = 32,
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

// TODO(fxbug.dev/117826): Unittest ValidateRingBufferVmo
// TEST(ValidateTest, ValidateRingBufferVmo) {}

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

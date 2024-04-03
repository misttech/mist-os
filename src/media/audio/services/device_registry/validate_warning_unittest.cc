// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <cmath>
#include <cstdint>
#include <optional>
#include <set>
#include <vector>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/signal_processing_utils.h"
#include "src/media/audio/services/device_registry/signal_processing_utils_unittest.h"
#include "src/media/audio/services/device_registry/validate.h"

// These cases unittest the Validate... functions with inputs that cause WARNING log output.

namespace media_audio {

// Negative-test ValidateStreamProperties
fuchsia_hardware_audio::StreamProperties ValidStreamProperties() {
  return {{
      .is_input = false,
      .min_gain_db = 0.0f,
      .max_gain_db = 0.0f,
      .gain_step_db = 0.0f,
      .plug_detect_capabilities = fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify,
      .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
  }};
}
TEST(ValidateWarningTest, BadStreamProperties) {
  auto stream_properties = ValidStreamProperties();
  ASSERT_EQ(ValidateStreamProperties(stream_properties), ZX_OK) << "Baseline setup unsuccessful";

  // missing is_input
  stream_properties = ValidStreamProperties();
  stream_properties.is_input() = std::nullopt;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);

  // missing min_gain_db
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = std::nullopt;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  // bad min_gain_db (NAN, inf)
  stream_properties.min_gain_db() = NAN;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  stream_properties.min_gain_db() = -INFINITY;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);

  // missing max_gain_db
  stream_properties = ValidStreamProperties();
  stream_properties.max_gain_db() = std::nullopt;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  // bad max_gain_db (NAN, inf)
  stream_properties.max_gain_db() = NAN;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  stream_properties.max_gain_db() = INFINITY;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);

  // bad max_gain_db (below min_gain_db)
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = 0.0f;
  stream_properties.max_gain_db() = -1.0f;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);

  // missing gain_step_db
  stream_properties = ValidStreamProperties();
  stream_properties.gain_step_db() = std::nullopt;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  // bad gain_step_db (NAN, inf)
  stream_properties.gain_step_db() = NAN;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  stream_properties.gain_step_db() = INFINITY;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  // gain_step_db too large (max-min)
  stream_properties.gain_step_db() = 1.0f;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = -42.0f;
  stream_properties.max_gain_db() = 26.0f;
  stream_properties.gain_step_db() = 68.1f;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);

  // current mute state impossible (implicit)
  stream_properties = ValidStreamProperties();
  EXPECT_EQ(
      ValidateStreamProperties(stream_properties,
                               fuchsia_hardware_audio::GainState{{.muted = true, .gain_db = 0.0f}}),
      ZX_ERR_INVALID_ARGS);
  // current mute state impossible (explicit)
  stream_properties.can_mute() = false;
  EXPECT_EQ(
      ValidateStreamProperties(stream_properties,
                               fuchsia_hardware_audio::GainState{{.muted = true, .gain_db = 0.0f}}),
      ZX_ERR_INVALID_ARGS);

  // current agc state impossible (implicit)
  stream_properties = ValidStreamProperties();
  EXPECT_EQ(ValidateStreamProperties(
                stream_properties,
                fuchsia_hardware_audio::GainState{{.agc_enabled = true, .gain_db = 0.0f}}),
            ZX_ERR_INVALID_ARGS);
  // current agc state impossible (explicit)
  stream_properties.can_agc() = false;
  EXPECT_EQ(ValidateStreamProperties(
                stream_properties,
                fuchsia_hardware_audio::GainState{{.agc_enabled = true, .gain_db = 0.0f}}),
            ZX_ERR_INVALID_ARGS);

  // current gain_db out of range
  stream_properties = ValidStreamProperties();
  EXPECT_EQ(ValidateStreamProperties(stream_properties,
                                     fuchsia_hardware_audio::GainState{{.gain_db = -0.1f}}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateStreamProperties(stream_properties,
                                     fuchsia_hardware_audio::GainState{{.gain_db = 0.1f}}),
            ZX_ERR_INVALID_ARGS);

  // missing plug_detect_capabilities
  stream_properties = ValidStreamProperties();
  stream_properties.plug_detect_capabilities() = std::nullopt;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);

  // current plug state impossible
  stream_properties = ValidStreamProperties();
  stream_properties.plug_detect_capabilities() =
      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired;
  EXPECT_EQ(ValidateStreamProperties(
                stream_properties, fuchsia_hardware_audio::GainState{{.gain_db = 0.0f}},
                fuchsia_hardware_audio::PlugState{{.plugged = false, .plug_state_time = 0}}),
            ZX_ERR_INVALID_ARGS);

  // missing clock_domain
  stream_properties = ValidStreamProperties();
  stream_properties.clock_domain() = std::nullopt;
  EXPECT_EQ(ValidateStreamProperties(stream_properties), ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormatSets
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
TEST(ValidateWarningTest, BadSupportedFormats) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> supported_formats;

  // Empty top-level vector
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.push_back(CompliantFormatSet());
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_OK);

  // No pcm_supported_formats (one supported_formats[] vector entry, but it is empty)
  supported_formats.emplace_back();
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormatSets for frame_rates
TEST(ValidateWarningTest, BadSupportedFormatsFrameRates) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing frame_rates
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = std::nullopt;
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Empty frame_rates vector
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Too low frame_rate
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{999}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_OUT_OF_RANGE);

  // Too high frame_rate
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{192001}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_OUT_OF_RANGE);

  // Out-of-order frame_rates
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{48000, 44100}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormatSets for channel_sets
TEST(ValidateWarningTest, BadSupportedFormatsChannelSets) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing channel_sets
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = std::nullopt;
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Empty channel_sets vector
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Missing attributes
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {},
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Empty attributes vector
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {
          .attributes = {{}},
      },
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Duplicate channel_set lengths
  // Two channel_sets entries - both with a single channel
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {{
          .attributes = {{
              {},
          }},
      }},
      {{
          .attributes = {{
              {},
          }},
      }},
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.at(0)
      .pcm_supported_formats()
      ->channel_sets()
      ->at(0)
      .attributes()
      ->emplace_back();
  ASSERT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_OK);

  // Too high min_frequency
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .min_frequency = 24001,
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_OUT_OF_RANGE);

  // Min > max
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .min_frequency = 16001,
      .max_frequency = 16000,
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Too high max_frequency (passes but emits WARNING, thus is in the "warning" suite)
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .max_frequency = 192000,
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_OK);
}

// Negative-test ValidateRingBufferFormatSets for sample_formats
TEST(ValidateWarningTest, BadSupportedFormatsSampleFormats) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> supported_formats{CompliantFormatSet()};
  // Missing sample_formats
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = std::nullopt;
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Empty sample_formats vector
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {{}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Duplicate sample_format
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {{
      fuchsia_hardware_audio::SampleFormat::kPcmSigned,
      fuchsia_hardware_audio::SampleFormat::kPcmSigned,
  }};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormatSets for bytes_per_sample
TEST(ValidateWarningTest, BadSupportedFormatsBytesPerSample) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing bytes_per_sample
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = std::nullopt;
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Empty bytes_per_sample vector
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Out-of-order bytes_per_sample
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{4, 2}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Bad bytes_per_sample - unsigned
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fuchsia_hardware_audio::SampleFormat::kPcmUnsigned}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{0, 1}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{1, 2}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Bad bytes_per_sample - signed
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fuchsia_hardware_audio::SampleFormat::kPcmSigned}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{1, 2}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{3, 4}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{2, 8}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Bad bytes_per_sample - float
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fuchsia_hardware_audio::SampleFormat::kPcmFloat}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{2, 4}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{6, 8}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{4, 16}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormatSets for valid_bits_per_sample
TEST(ValidateWarningTest, BadSupportedFormatsValidBitsPerSample) {
  std::vector<fuchsia_hardware_audio::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = std::nullopt;
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Empty valid_bits_per_sample vector
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Out-of-order valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{16, 15}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_INVALID_ARGS);

  // Too low valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{0, 16}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_OUT_OF_RANGE);

  // Too high valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{16, 18}};
  EXPECT_EQ(ValidateRingBufferFormatSets(supported_formats), ZX_ERR_OUT_OF_RANGE);
}

// Negative-test ValidateGainState
TEST(ValidateWarningTest, BadGainState) {
  // empty
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{}), ZX_ERR_INVALID_ARGS);

  // missing gain_db
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = false,
                              }},
                              std::nullopt),
            ZX_ERR_INVALID_ARGS);

  //  bad gain_db
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = false,
                                  .gain_db = NAN,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  .can_mute = true,
                                  .can_agc = true,
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = false,
                                  .gain_db = INFINITY,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  .can_mute = true,
                                  .can_agc = true,
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_INVALID_ARGS);

  // gain_db out-of-range
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = false,
                                  .gain_db = -12.1f,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  .can_mute = true,
                                  .can_agc = true,
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = false,
                                  .gain_db = 12.1f,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  .can_mute = true,
                                  .can_agc = true,
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_OUT_OF_RANGE);

  // bad muted (implicit)
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = true,
                                  .agc_enabled = false,
                                  .gain_db = 0.0f,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  // can_mute (optional) is missing: CANNOT mute
                                  .can_agc = true,
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_INVALID_ARGS);

  // bad muted (explicit)
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = true,
                                  .agc_enabled = false,
                                  .gain_db = 0.0f,
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
            ZX_ERR_INVALID_ARGS);

  // bad agc_enabled (implicit)
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = true,
                                  .gain_db = 0.0f,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  .can_mute = true,
                                  // can_agc ia missing: CANNOT agc
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_INVALID_ARGS);

  // bad agc_enabled (explicit)
  EXPECT_EQ(ValidateGainState(fuchsia_hardware_audio::GainState{{
                                  .muted = false,
                                  .agc_enabled = true,
                                  .gain_db = 0.0f,
                              }},
                              fuchsia_hardware_audio::StreamProperties{{
                                  .is_input = false,
                                  .can_mute = true,
                                  .can_agc = false,
                                  .min_gain_db = -12.0f,
                                  .max_gain_db = 12.0f,
                                  .gain_step_db = 0.5f,
                                  .plug_detect_capabilities =
                                      fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired,
                                  .clock_domain = fuchsia_hardware_audio::kClockDomainMonotonic,
                              }}),
            ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidatePlugState
TEST(ValidateWarningTest, BadPlugState) {
  // empty
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{}), ZX_ERR_INVALID_ARGS);

  // missing plugged
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                  // plugged (required) is missing
                                  .plug_state_time = zx::clock::get_monotonic().get(),
                              }},
                              fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify),
            ZX_ERR_INVALID_ARGS);

  // bad plugged
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                  .plugged = false,
                                  .plug_state_time = zx::clock::get_monotonic().get(),
                              }},
                              fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired),
            ZX_ERR_INVALID_ARGS);

  // missing plug_state_time
  EXPECT_EQ(ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                                  .plugged = false,
                                  // plug_state_time (required) is missing
                              }},
                              fuchsia_hardware_audio::PlugDetectCapabilities::kCanAsyncNotify),
            ZX_ERR_INVALID_ARGS);

  // bad plug_state_time
  EXPECT_EQ(
      ValidatePlugState(fuchsia_hardware_audio::PlugState{{
                            .plugged = true,
                            .plug_state_time = (zx::clock::get_monotonic() + zx::hour(6)).get(),
                        }},
                        fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired),
      ZX_ERR_INVALID_ARGS);
}

// TODO(https://fxbug.dev/42069012): Negative-test ValidateDeviceInfo
// TEST(ValidateWarningTest, BadDeviceInfo) {}

// Negative-test ValidateRingBufferProperties
TEST(ValidateWarningTest, BadRingBufferProperties) {
  // empty
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{}),
            ZX_ERR_INVALID_ARGS);

  // missing needs_cache_flush_or_invalidate
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .turn_on_delay = 125,
                .driver_transfer_bytes = 128,
            }}),
            ZX_ERR_INVALID_ARGS);

  // bad turn_on_delay
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = -1,
                .driver_transfer_bytes = 128,
            }}),
            ZX_ERR_OUT_OF_RANGE);

  // missing driver_transfer_bytes
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = 125,
            }}),
            ZX_ERR_INVALID_ARGS);

  // TODO(b/311694769): Resolve driver_transfer_bytes lower limit: specifically is 0 allowed?
  // bad driver_transfer_bytes (too small)
  // EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0,
  //           }}),
  //           ZX_ERR_INVALID_ARGS);

  // TODO(b/311694769): Resolve driver_transfer_bytes upper limit: no limit? Soft guideline?
  // bad driver_transfer_bytes (too large)
  // EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0xFFFFFFFF,
  //           }}),
  //           ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormat
TEST(ValidateWarningTest, BadRingBufferFormat) {
  // missing pcm_format
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{}), ZX_ERR_INVALID_ARGS);

  // bad value number_of_channels
  // Is there an upper limit on number_of_channels?
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 0,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 2,
                    .valid_bits_per_sample = 16,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);

  // bad value bytes_per_sample
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 0,
                    .valid_bits_per_sample = 16,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 5,
                    .valid_bits_per_sample = 16,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);

  // bad value valid_bits_per_sample
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 2,
                    .valid_bits_per_sample = 0,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmUnsigned,
                    .bytes_per_sample = 1,
                    .valid_bits_per_sample = 9,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 2,
                    .valid_bits_per_sample = 17,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 4,
                    .valid_bits_per_sample = 33,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmFloat,
                    .bytes_per_sample = 4,
                    .valid_bits_per_sample = 33,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmFloat,
                    .bytes_per_sample = 8,
                    .valid_bits_per_sample = 65,
                    .frame_rate = 48000,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);

  // bad value frame_rate
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 2,
                    .valid_bits_per_sample = 16,
                    .frame_rate = 999,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
                .pcm_format = fuchsia_hardware_audio::PcmFormat{{
                    .number_of_channels = 2,
                    .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                    .bytes_per_sample = 2,
                    .valid_bits_per_sample = 16,
                    .frame_rate = 192001,
                }},
            }}),
            ZX_ERR_OUT_OF_RANGE);
}

// Negative-test ValidateSampleFormatCompatibility
TEST(ValidateWarningTest, BadFormatCompatibility) {
  const std::set<std::pair<uint8_t, fuchsia_hardware_audio::SampleFormat>> kAllowedFormats{
      {1, fuchsia_hardware_audio::SampleFormat::kPcmUnsigned},
      {2, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
      {4, fuchsia_hardware_audio::SampleFormat::kPcmSigned},
      {4, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
      {8, fuchsia_hardware_audio::SampleFormat::kPcmFloat},
  };
  const std::vector<uint8_t> kSampleSizesToTest{
      0, 1, 2, 3, 4, 6, 8,
  };
  const std::vector<fuchsia_hardware_audio::SampleFormat> kSampleFormatsToTest{
      fuchsia_hardware_audio::SampleFormat::kPcmUnsigned,
      fuchsia_hardware_audio::SampleFormat::kPcmSigned,
      fuchsia_hardware_audio::SampleFormat::kPcmFloat,
  };

  for (auto sample_size : kSampleSizesToTest) {
    for (auto sample_format : kSampleFormatsToTest) {
      if (kAllowedFormats.find({sample_size, sample_format}) == kAllowedFormats.end()) {
        EXPECT_EQ(ValidateSampleFormatCompatibility(sample_size, sample_format),
                  ZX_ERR_INVALID_ARGS);
      }
    }
  }
}

// Negative-test ValidateRingBufferVmo
TEST(ValidateWarningTest, BadRingBufferVmo) {
  constexpr uint64_t kVmoContentSize = 8192;
  zx::vmo vmo;
  auto status = zx::vmo::create(kVmoContentSize, 0, &vmo);
  ASSERT_EQ(status, ZX_OK) << "could not create VMO for test input";

  constexpr uint8_t kChannelCount = 1;
  constexpr uint8_t kSampleSize = 2;
  fuchsia_hardware_audio::Format format{{
      .pcm_format = fuchsia_hardware_audio::PcmFormat{{
          .number_of_channels = kChannelCount,
          .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
          .bytes_per_sample = kSampleSize,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }};
  uint32_t num_frames = static_cast<uint32_t>(kVmoContentSize / kChannelCount / kSampleSize);

  // Bad VMO (get_size failed)
  EXPECT_EQ(ValidateRingBufferVmo(zx::vmo(), num_frames, format), ZX_ERR_BAD_HANDLE);

  // bad num_frames (too large for VMO)
  EXPECT_EQ(ValidateRingBufferVmo(vmo, num_frames + 1, format), ZX_ERR_INVALID_ARGS);

  // Bad format (flagged by the encapsulated ValidateRingBufferFormat)
  format.pcm_format()->frame_rate() = 999;
  EXPECT_EQ(ValidateRingBufferVmo(vmo, num_frames, format), ZX_ERR_OUT_OF_RANGE);
  format.pcm_format()->frame_rate() = 192001;
  EXPECT_EQ(ValidateRingBufferVmo(vmo, num_frames, format), ZX_ERR_OUT_OF_RANGE);

  // Bad format (flagged by the encapsulated ValidateSampleFormatCompatibility)
  format.pcm_format()->frame_rate() = 48000;
  format.pcm_format()->sample_format() = fuchsia_hardware_audio::SampleFormat::kPcmFloat;
  EXPECT_EQ(ValidateRingBufferVmo(vmo, num_frames, format), ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateDelayInfo for internal_delay
TEST(ValidateWarningTest, BadInternalDelayInfo) {
  // empty
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{}), ZX_ERR_INVALID_ARGS);

  // missing internal_delay
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                .external_delay = 0,
            }}),
            ZX_ERR_INVALID_ARGS);

  // bad internal_delay
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                .internal_delay = -1,
            }}),
            ZX_ERR_OUT_OF_RANGE);
}

// Negative-test ValidateDelayInfo for external_delay
TEST(ValidateWarningTest, BadExternalDelayInfo) {
  // bad external_delay
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                .internal_delay = 0,
                .external_delay = -1,
            }}),
            ZX_ERR_OUT_OF_RANGE);
}

// Unittest ValidateCodecProperties -- the missing, minimal and maximal possibilities
TEST(ValidateWarningTest, BadCodecProperties) {
  EXPECT_EQ(ValidateCodecProperties(fuchsia_hardware_audio::CodecProperties{{
                .is_input = false,
                .manufacturer = "manufacturer",
                .product = "product",
                .unique_id = {{}},
                // plug_detect_capabilities missing
            }}),
            ZX_ERR_INVALID_ARGS)
      << "missing plug_detect_capabilities";
}

// Unittest ValidateDaiFormatSets
TEST(ValidateWarningTest, BadDaiSupportedFormats) {
  // Entirely empty
  EXPECT_EQ(ValidateDaiFormatSets(std::vector<fuchsia_hardware_audio::DaiSupportedFormats>{}),
            ZX_ERR_INVALID_ARGS);

  // each empty
  EXPECT_EQ(
      ValidateDaiFormatSets({{
          {{
              // .number_of_channels = {1},
              .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
              .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
              .frame_rates = {48000},
              .bits_per_slot = {32},
              .bits_per_sample = {16},
          }},
      }}),
      ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(
      ValidateDaiFormatSets({{
          {{
              .number_of_channels = {1},
              // .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
              .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
              .frame_rates = {48000},
              .bits_per_slot = {32},
              .bits_per_sample = {16},
          }},
      }}),
      ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(
      ValidateDaiFormatSets({{
          {{
              .number_of_channels = {1},
              .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
              // .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
              //     fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
              .frame_rates = {48000},
              .bits_per_slot = {32},
              .bits_per_sample = {16},
          }},
      }}),
      ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(
      ValidateDaiFormatSets({{
          {{
              .number_of_channels = {1},
              .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
              .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
              // .frame_rates = {48000},
              .bits_per_slot = {32},
              .bits_per_sample = {16},
          }},
      }}),
      ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(
      ValidateDaiFormatSets({{
          {{
              .number_of_channels = {1},
              .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
              .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
              .frame_rates = {48000},
              // .bits_per_slot = {32},
              .bits_per_sample = {16},
          }},
      }}),
      ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(
      ValidateDaiFormatSets({{
          {{
              .number_of_channels = {1},
              .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
              .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                  fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
              .frame_rates = {48000},
              .bits_per_slot = {32},
              // .bits_per_sample = {16},
          }},
      }}),
      ZX_ERR_INVALID_ARGS);

  const fuchsia_hardware_audio::DaiSupportedFormats valid = {{
      .number_of_channels = {1},
      .sample_formats = {fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned},
      .frame_formats = {fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S)},
      .frame_rates = {48000},
      .bits_per_slot = {32},
      .bits_per_sample = {16},
  }};

  // values too small
  fuchsia_hardware_audio::DaiSupportedFormats fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.number_of_channels({0, 1, 2}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.frame_rates({0, 48000}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.bits_per_slot({0, 32}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.bits_per_sample({0, 16}),
            }}),
            ZX_ERR_INVALID_ARGS);

  // values too large
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.number_of_channels({1, 2, 65}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.frame_rates({48000, 2'000'000'000}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.bits_per_slot({32, 65}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.bits_per_sample({16, 33}),
            }}),
            ZX_ERR_INVALID_ARGS);

  // values out of order
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.number_of_channels({2, 1}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.frame_rates({48000, 44100}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.bits_per_slot({32, 16}),
            }}),
            ZX_ERR_INVALID_ARGS);
  fmts = valid;
  EXPECT_EQ(ValidateDaiFormatSets({{
                fmts.bits_per_sample({16, 8}),
            }}),
            ZX_ERR_INVALID_ARGS);
}

// Unittest ValidateDaiFormat
TEST(ValidateWarningTest, BadDaiFormat) {
  // empty
  EXPECT_EQ(ValidateDaiFormat({{}}), ZX_ERR_INVALID_ARGS);

  // each missing
  EXPECT_EQ(ValidateDaiFormat({{
                // .number_of_channels = 2,
                .channels_to_use_bitmask = 0x03,
                .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
                .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                    fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
                .frame_rate = 48000,
                .bits_per_slot = 32,
                .bits_per_sample = 16,
            }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateDaiFormat({{
                .number_of_channels = 2,
                // .channels_to_use_bitmask = 0x03,
                .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
                .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                    fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
                .frame_rate = 48000,
                .bits_per_slot = 32,
                .bits_per_sample = 16,
            }}),
            ZX_ERR_INVALID_ARGS);

  EXPECT_EQ(ValidateDaiFormat({{
                .number_of_channels = 2,
                .channels_to_use_bitmask = 0x03,
                // .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
                .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                    fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
                .frame_rate = 48000,
                .bits_per_slot = 32,
                .bits_per_sample = 16,
            }}),
            ZX_ERR_INVALID_ARGS);

  // Missing FrameFormat is impossible since DaiFrameFormat (and thus DaiFormat) has a custom ctor.

  EXPECT_EQ(ValidateDaiFormat({{
                .number_of_channels = 2,
                .channels_to_use_bitmask = 0x03,
                .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
                .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                    fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
                // .frame_rate = 48000,
                .bits_per_slot = 32,
                .bits_per_sample = 16,
            }}),
            ZX_ERR_INVALID_ARGS);

  EXPECT_EQ(ValidateDaiFormat({{
                .number_of_channels = 2,
                .channels_to_use_bitmask = 0x03,
                .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
                .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                    fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
                .frame_rate = 48000,
                // .bits_per_slot = 32,
                .bits_per_sample = 16,
            }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateDaiFormat({{
                .number_of_channels = 2,
                .channels_to_use_bitmask = 0x03,
                .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
                .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
                    fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
                .frame_rate = 48000,
                .bits_per_slot = 32,
                // .bits_per_sample = 16,
            }}),
            ZX_ERR_INVALID_ARGS);

  // Values too low
  const fuchsia_hardware_audio::DaiFormat valid = {{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }};
  fuchsia_hardware_audio::DaiFormat fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.number_of_channels(0)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.channels_to_use_bitmask(0x00)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.frame_rate(0)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.bits_per_slot(0).bits_per_sample(0)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.bits_per_sample(0)), ZX_ERR_INVALID_ARGS);

  // values too large
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.number_of_channels(65)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.channels_to_use_bitmask(0x04)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.frame_rate(2'000'000'000)), ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.bits_per_slot(kMaxSupportedDaiFormatBitsPerSlot + 1)),
            ZX_ERR_INVALID_ARGS);
  fmt = valid;
  EXPECT_EQ(ValidateDaiFormat(fmt.bits_per_sample(33)), ZX_ERR_INVALID_ARGS);
}

// Unittest ValidateCodecFormatInfo
TEST(ValidateWarningTest, BadCodecFormatInfo) {
  // These durations cannot be negative.
  EXPECT_EQ(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
                .external_delay = -1,
            }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
                .turn_on_delay = -1,
            }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
                .turn_off_delay = -1,
            }}),
            ZX_ERR_INVALID_ARGS);
  // ...that includes INT64_MIN (check for erroneously treating it as unsigned).
  EXPECT_EQ(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
                .external_delay = zx::time::infinite_past().get(),
            }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
                .turn_on_delay = zx::time::infinite_past().get(),
            }}),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateCodecFormatInfo(fuchsia_hardware_audio::CodecFormatInfo{{
                .turn_off_delay = zx::time::infinite_past().get(),
            }}),
            ZX_ERR_INVALID_ARGS);
}

// signalprocessing functions
//
TEST(ValidateWarningTest, BadElement) {
  // This element has no 'id'.
  EXPECT_EQ(ValidateElement(kElementNoId), ZX_ERR_INVALID_ARGS);

  // This element has no 'type'.
  EXPECT_EQ(ValidateElement(kElementNoType), ZX_ERR_INVALID_ARGS);

  // This element has no 'type_specific', but its 'type' requires one.
  EXPECT_EQ(ValidateElement(kElementNoRequiredTypeSpecific), ZX_ERR_INVALID_ARGS);

  // This element contains a 'type_specific' that does not match its 'type'.
  EXPECT_EQ(ValidateElement(kElementWrongTypeSpecific), ZX_ERR_INVALID_ARGS);

  // This element contains a 'description' that is an empty string.
  EXPECT_EQ(ValidateElement(kElementEmptyDescription), ZX_ERR_INVALID_ARGS);

  // Test inconsistencies in certain type_specifics
  // TODO(https://fxbug.dev/42069012): Negative-test ValidateElement
}

TEST(ValidateWarningTest, BadElementList) {
  EXPECT_EQ(ValidateElements(kEmptyElements), ZX_ERR_INVALID_ARGS);

  // List contains two elements with the same id.
  EXPECT_EQ(ValidateElements(kElementsDuplicateId), ZX_ERR_INVALID_ARGS);

  // bad Elements: all the ValidateElement negative cases
  EXPECT_EQ(ValidateElements(kElementsWithNoId), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateElements(kElementsWithNoType), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateElements(kElementsWithNoRequiredTypeSpecific), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateElements(kElementsWithWrongTypeSpecific), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateElements(kElementsWithEmptyDescription), ZX_ERR_INVALID_ARGS);
}

TEST(ValidateWarningTest, BadTopology) {
  // This topology has no 'id'.
  EXPECT_EQ(ValidateTopology(kTopologyMissingId, MapElements(kElements)), ZX_ERR_INVALID_ARGS);

  // This topology has no 'processing_elements_edge_pairs'.
  EXPECT_EQ(ValidateTopology(kTopologyMissingEdgePairs, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);

  // This topology has an 'processing_elements_edge_pairs' vector that is empty.
  EXPECT_EQ(ValidateTopology(kTopologyEmptyEdgePairs, MapElements(kElements)), ZX_ERR_INVALID_ARGS);

  // This topology references an element_id that is not included in the element_map.
  EXPECT_EQ(ValidateTopology(kTopologyUnknownElementId, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);

  // This topology includes an edge that connects one element_id to itself.
  EXPECT_EQ(ValidateTopology(kTopologyEdgePairLoop, MapElements(kElements)), ZX_ERR_INVALID_ARGS);

  // This topology has a terminal (source or destination) element that is not an Endpoint.
  EXPECT_EQ(ValidateTopology(kTopologyTerminalNotEndpoint, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);

  // empty element_map
  EXPECT_EQ(ValidateTopology(kTopology14, kEmptyElementMap), ZX_ERR_INVALID_ARGS);
}

TEST(ValidateWarningTest, BadTopologyList) {
  EXPECT_EQ(ValidateTopologies(kEmptyTopologies, MapElements(kElements)), ZX_ERR_INVALID_ARGS);

  // List contains two topologies with the same id.
  EXPECT_EQ(ValidateTopologies(kTopologiesWithDuplicateId, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);

  // There are elements that are not mentioned in at least one of the topologies.
  EXPECT_EQ(ValidateTopologies(kTopologiesWithoutAllElements, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);

  // Topology list with a bad Topology: all the ValidateTopology negative cases
  EXPECT_EQ(ValidateTopologies(kTopologiesWithMissingId, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateTopologies(kTopologiesWithMissingEdgePairs, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateTopologies(kTopologiesWithEmptyEdgePairs, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateTopologies(kTopologiesWithUnknownElementId, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateTopologies(kTopologiesWithLoop, MapElements(kElements)), ZX_ERR_INVALID_ARGS);
  EXPECT_EQ(ValidateTopologies(kTopologiesWithTerminalNotEndpoint, MapElements(kElements)),
            ZX_ERR_INVALID_ARGS);

  // empty element_map
  EXPECT_EQ(ValidateTopologies(kTopologies, kEmptyElementMap), ZX_ERR_INVALID_ARGS);
}

TEST(ValidateWarningTest, BadElementState) {
  EXPECT_EQ(ValidateElementState(kElementStateEmpty, kElement1), ZX_ERR_INVALID_ARGS);

  // Add more negative-test cases here
}

}  // namespace media_audio

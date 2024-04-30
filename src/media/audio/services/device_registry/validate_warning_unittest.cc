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
namespace {

namespace fha = fuchsia_hardware_audio;

// Negative-test ValidateStreamProperties
fha::StreamProperties ValidStreamProperties() {
  return {{
      .is_input = false,
      .min_gain_db = 0.0f,
      .max_gain_db = 0.0f,
      .gain_step_db = 0.0f,
      .plug_detect_capabilities = fha::PlugDetectCapabilities::kCanAsyncNotify,
      .clock_domain = fha::kClockDomainMonotonic,
  }};
}
TEST(ValidateWarningTest, BadStreamProperties) {
  auto stream_properties = ValidStreamProperties();
  ASSERT_TRUE(ValidateStreamProperties(stream_properties)) << "Baseline setup unsuccessful";

  // missing is_input
  stream_properties = ValidStreamProperties();
  stream_properties.is_input() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // missing min_gain_db
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // bad min_gain_db (NAN, inf)
  stream_properties.min_gain_db() = NAN;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties.min_gain_db() = -INFINITY;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // missing max_gain_db
  stream_properties = ValidStreamProperties();
  stream_properties.max_gain_db() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // bad max_gain_db (NAN, inf)
  stream_properties.max_gain_db() = NAN;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties.max_gain_db() = INFINITY;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // bad max_gain_db (below min_gain_db)
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = 0.0f;
  stream_properties.max_gain_db() = -1.0f;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // missing gain_step_db
  stream_properties = ValidStreamProperties();
  stream_properties.gain_step_db() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // bad gain_step_db (NAN, inf)
  stream_properties.gain_step_db() = NAN;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties.gain_step_db() = INFINITY;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  // gain_step_db too large (max-min)
  stream_properties.gain_step_db() = 1.0f;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
  stream_properties = ValidStreamProperties();
  stream_properties.min_gain_db() = -42.0f;
  stream_properties.max_gain_db() = 26.0f;
  stream_properties.gain_step_db() = 68.1f;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // current mute state impossible (implicit)
  stream_properties = ValidStreamProperties();
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.muted = true, .gain_db = 0.0f}}));
  // current mute state impossible (explicit)
  stream_properties.can_mute() = false;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.muted = true, .gain_db = 0.0f}}));

  // current agc state impossible (implicit)
  stream_properties = ValidStreamProperties();
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.agc_enabled = true, .gain_db = 0.0f}}));
  // current agc state impossible (explicit)
  stream_properties.can_agc() = false;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties,
                                        fha::GainState{{.agc_enabled = true, .gain_db = 0.0f}}));

  // current gain_db out of range
  stream_properties = ValidStreamProperties();
  EXPECT_FALSE(ValidateStreamProperties(stream_properties, fha::GainState{{.gain_db = -0.1f}}));
  EXPECT_FALSE(ValidateStreamProperties(stream_properties, fha::GainState{{.gain_db = 0.1f}}));

  // missing plug_detect_capabilities
  stream_properties = ValidStreamProperties();
  stream_properties.plug_detect_capabilities() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));

  // current plug state impossible
  stream_properties = ValidStreamProperties();
  stream_properties.plug_detect_capabilities() = fha::PlugDetectCapabilities::kHardwired;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties, fha::GainState{{.gain_db = 0.0f}},
                                        fha::PlugState{{.plugged = false, .plug_state_time = 0}}));

  // missing clock_domain
  stream_properties = ValidStreamProperties();
  stream_properties.clock_domain() = std::nullopt;
  EXPECT_FALSE(ValidateStreamProperties(stream_properties));
}

// Negative-test ValidateRingBufferFormatSets
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
TEST(ValidateWarningTest, BadSupportedFormats) {
  std::vector<fha::SupportedFormats> supported_formats;

  // Empty top-level vector
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.push_back(CompliantFormatSet());
  EXPECT_TRUE(ValidateRingBufferFormatSets(supported_formats));

  // No pcm_supported_formats (one supported_formats[] vector entry, but it is empty)
  supported_formats.emplace_back();
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for frame_rates
TEST(ValidateWarningTest, BadSupportedFormatsFrameRates) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing frame_rates
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty frame_rates vector
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too low frame_rate
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{999}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too high frame_rate
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{192001}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Out-of-order frame_rates
  supported_formats.at(0).pcm_supported_formats()->frame_rates() = {{48000, 44100}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for channel_sets
TEST(ValidateWarningTest, BadSupportedFormatsChannelSets) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing channel_sets
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty channel_sets vector
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Missing attributes
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {},
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty attributes vector
  supported_formats.at(0).pcm_supported_formats()->channel_sets() = {{
      {
          .attributes = {{}},
      },
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

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
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0)
      .pcm_supported_formats()
      ->channel_sets()
      ->at(0)
      .attributes()
      ->emplace_back();
  ASSERT_TRUE(ValidateRingBufferFormatSets(supported_formats));

  // Too high min_frequency
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .min_frequency = 24001,
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Min > max
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .min_frequency = 16001,
      .max_frequency = 16000,
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too high max_frequency (passes but emits WARNING, thus is in the "warning" suite)
  supported_formats.at(0).pcm_supported_formats()->channel_sets()->at(1).attributes()->at(0) = {{
      .max_frequency = 192000,
  }};
  EXPECT_TRUE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for sample_formats
TEST(ValidateWarningTest, BadSupportedFormatsSampleFormats) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};
  // Missing sample_formats
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty sample_formats vector
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Duplicate sample_format
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {{
      fha::SampleFormat::kPcmSigned,
      fha::SampleFormat::kPcmSigned,
  }};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for bytes_per_sample
TEST(ValidateWarningTest, BadSupportedFormatsBytesPerSample) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing bytes_per_sample
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty bytes_per_sample vector
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Out-of-order bytes_per_sample
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{4, 2}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Bad bytes_per_sample - unsigned
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fha::SampleFormat::kPcmUnsigned}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{0, 1}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{1, 2}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Bad bytes_per_sample - signed
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fha::SampleFormat::kPcmSigned}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{1, 2}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{3, 4}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{2, 8}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Bad bytes_per_sample - float
  supported_formats.at(0).pcm_supported_formats()->sample_formats() = {
      {fha::SampleFormat::kPcmFloat}};
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{2, 4}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{6, 8}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
  supported_formats.at(0).pcm_supported_formats()->bytes_per_sample() = {{4, 16}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateRingBufferFormatSets for valid_bits_per_sample
TEST(ValidateWarningTest, BadSupportedFormatsValidBitsPerSample) {
  std::vector<fha::SupportedFormats> supported_formats{CompliantFormatSet()};

  // Missing valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = std::nullopt;
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Empty valid_bits_per_sample vector
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Out-of-order valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{16, 15}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too low valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{0, 16}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));

  // Too high valid_bits_per_sample
  supported_formats.at(0).pcm_supported_formats()->valid_bits_per_sample() = {{16, 18}};
  EXPECT_FALSE(ValidateRingBufferFormatSets(supported_formats));
}

// Negative-test ValidateGainState
TEST(ValidateWarningTest, BadGainState) {
  // empty
  EXPECT_FALSE(ValidateGainState(fha::GainState{}));

  // missing gain_db
  EXPECT_FALSE(ValidateGainState(fha::GainState{{
                                     .muted = false,
                                     .agc_enabled = false,
                                 }},
                                 std::nullopt));

  //  bad gain_db
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = NAN,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = INFINITY,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // gain_db out-of-range
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = -12.1f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = false,
                            .gain_db = 12.1f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad muted (implicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = true,
                            .agc_enabled = false,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            // can_mute (optional) is missing: CANNOT mute
                            .can_agc = true,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad muted (explicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = true,
                            .agc_enabled = false,
                            .gain_db = 0.0f,
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

  // bad agc_enabled (implicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = true,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            // can_agc ia missing: CANNOT agc
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));

  // bad agc_enabled (explicit)
  EXPECT_FALSE(
      ValidateGainState(fha::GainState{{
                            .muted = false,
                            .agc_enabled = true,
                            .gain_db = 0.0f,
                        }},
                        fha::StreamProperties{{
                            .is_input = false,
                            .can_mute = true,
                            .can_agc = false,
                            .min_gain_db = -12.0f,
                            .max_gain_db = 12.0f,
                            .gain_step_db = 0.5f,
                            .plug_detect_capabilities = fha::PlugDetectCapabilities::kHardwired,
                            .clock_domain = fha::kClockDomainMonotonic,
                        }}));
}

// Negative-test ValidatePlugState
TEST(ValidateWarningTest, BadPlugState) {
  // empty
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{}));

  // missing plugged
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{{
                                     // plugged (required) is missing
                                     .plug_state_time = zx::clock::get_monotonic().get(),
                                 }},
                                 fha::PlugDetectCapabilities::kCanAsyncNotify));

  // bad plugged
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{{
                                     .plugged = false,
                                     .plug_state_time = zx::clock::get_monotonic().get(),
                                 }},
                                 fha::PlugDetectCapabilities::kHardwired));

  // missing plug_state_time
  EXPECT_FALSE(ValidatePlugState(fha::PlugState{{
                                     .plugged = false,
                                     // plug_state_time (required) is missing
                                 }},
                                 fha::PlugDetectCapabilities::kCanAsyncNotify));

  // bad plug_state_time
  EXPECT_FALSE(
      ValidatePlugState(fha::PlugState{{
                            .plugged = true,
                            .plug_state_time = (zx::clock::get_monotonic() + zx::hour(6)).get(),
                        }},
                        fha::PlugDetectCapabilities::kHardwired));
}

// TODO(https://fxbug.dev/42069012): Negative-test ValidateDeviceInfo
// TEST(ValidateWarningTest, BadDeviceInfo) {}

// Negative-test ValidateRingBufferProperties
TEST(ValidateWarningTest, BadRingBufferProperties) {
  // empty
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{}));

  // missing needs_cache_flush_or_invalidate
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .turn_on_delay = 125,
      .driver_transfer_bytes = 128,
  }}));

  // bad turn_on_delay
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      .turn_on_delay = -1,
      .driver_transfer_bytes = 128,
  }}));

  // missing driver_transfer_bytes
  EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
      .needs_cache_flush_or_invalidate = true,
      .turn_on_delay = 125,
  }}));

  // TODO(b/311694769): Resolve driver_transfer_bytes lower limit: specifically is 0 allowed?
  // bad driver_transfer_bytes (too small)
  // EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0,
  //           }}),
  //           ZX_ERR_INVALID_ARGS);

  // TODO(b/311694769): Resolve driver_transfer_bytes upper limit: no limit? Soft guideline?
  // bad driver_transfer_bytes (too large)
  // EXPECT_FALSE(ValidateRingBufferProperties(fha::RingBufferProperties{{
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0xFFFFFFFF,
  //           }}),
  //           ZX_ERR_INVALID_ARGS);
}

// Negative-test ValidateRingBufferFormat
TEST(ValidateWarningTest, BadRingBufferFormat) {
  // missing pcm_format
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{}));

  // bad value number_of_channels
  // Is there an upper limit on number_of_channels?
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 0,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }}));

  // bad value bytes_per_sample
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 0,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 5,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }}));

  // bad value valid_bits_per_sample
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 0,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmUnsigned,
          .bytes_per_sample = 1,
          .valid_bits_per_sample = 9,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 17,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 4,
          .valid_bits_per_sample = 33,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmFloat,
          .bytes_per_sample = 4,
          .valid_bits_per_sample = 33,
          .frame_rate = 48000,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmFloat,
          .bytes_per_sample = 8,
          .valid_bits_per_sample = 65,
          .frame_rate = 48000,
      }},
  }}));

  // bad value frame_rate
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 16,
          .frame_rate = 999,
      }},
  }}));
  EXPECT_FALSE(ValidateRingBufferFormat(fha::Format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = 16,
          .frame_rate = 192001,
      }},
  }}));
}

// Negative-test ValidateSampleFormatCompatibility
TEST(ValidateWarningTest, BadFormatCompatibility) {
  const std::set<std::pair<uint8_t, fha::SampleFormat>> kAllowedFormats{
      {1, fha::SampleFormat::kPcmUnsigned}, {2, fha::SampleFormat::kPcmSigned},
      {4, fha::SampleFormat::kPcmSigned},   {4, fha::SampleFormat::kPcmFloat},
      {8, fha::SampleFormat::kPcmFloat},
  };
  const std::vector<uint8_t> kSampleSizesToTest{
      0, 1, 2, 3, 4, 6, 8,
  };
  const std::vector<fha::SampleFormat> kSampleFormatsToTest{
      fha::SampleFormat::kPcmUnsigned,
      fha::SampleFormat::kPcmSigned,
      fha::SampleFormat::kPcmFloat,
  };

  for (auto sample_size : kSampleSizesToTest) {
    for (auto sample_format : kSampleFormatsToTest) {
      if (kAllowedFormats.find({sample_size, sample_format}) == kAllowedFormats.end()) {
        EXPECT_FALSE(ValidateSampleFormatCompatibility(sample_size, sample_format));
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
  fha::Format format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels = kChannelCount,
          .sample_format = fha::SampleFormat::kPcmSigned,
          .bytes_per_sample = kSampleSize,
          .valid_bits_per_sample = 16,
          .frame_rate = 48000,
      }},
  }};
  uint32_t num_frames = static_cast<uint32_t>(kVmoContentSize / kChannelCount / kSampleSize);

  // Bad VMO (get_size failed)
  EXPECT_FALSE(ValidateRingBufferVmo(zx::vmo(), num_frames, format));

  // bad num_frames (too large for VMO)
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames + 1, format));

  // Bad format (flagged by the encapsulated ValidateRingBufferFormat)
  format.pcm_format()->frame_rate() = 999;
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames, format));
  format.pcm_format()->frame_rate() = 192001;
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames, format));

  // Bad format (flagged by the encapsulated ValidateSampleFormatCompatibility)
  format.pcm_format()->frame_rate() = 48000;
  format.pcm_format()->sample_format() = fha::SampleFormat::kPcmFloat;
  EXPECT_FALSE(ValidateRingBufferVmo(vmo, num_frames, format));
}

// Negative-test ValidateDelayInfo for internal_delay
TEST(ValidateWarningTest, BadInternalDelayInfo) {
  // empty
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{}));

  // missing internal_delay
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{{
      .external_delay = 0,
  }}));

  // bad internal_delay
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = -1,
  }}));
}

// Negative-test ValidateDelayInfo for external_delay
TEST(ValidateWarningTest, BadExternalDelayInfo) {
  // bad external_delay
  EXPECT_FALSE(ValidateDelayInfo(fha::DelayInfo{{
      .internal_delay = 0,
      .external_delay = -1,
  }}));
}

// Unittest ValidateCodecProperties -- the missing, minimal and maximal possibilities
TEST(ValidateWarningTest, BadCodecProperties) {
  EXPECT_FALSE(ValidateCodecProperties(fha::CodecProperties{{
      .is_input = false, .manufacturer = "manufacturer", .product = "product", .unique_id = {{}},
      // plug_detect_capabilities missing
  }})) << "missing plug_detect_capabilities";
}

// Unittest ValidateDaiFormatSets
TEST(ValidateWarningTest, BadDaiSupportedFormats) {
  // Entirely empty
  EXPECT_FALSE(ValidateDaiFormatSets(std::vector<fha::DaiSupportedFormats>{}));

  // each empty
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          // .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          // .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          // .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
          //     fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          // .frame_rates = {48000},
          .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          // .bits_per_slot = {32},
          .bits_per_sample = {16},
      }},
  }}));
  EXPECT_FALSE(ValidateDaiFormatSets({{
      {{
          .number_of_channels = {1},
          .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
          .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
              fha::DaiFrameFormatStandard::kI2S)},
          .frame_rates = {48000},
          .bits_per_slot = {32},
          // .bits_per_sample = {16},
      }},
  }}));

  const fha::DaiSupportedFormats valid = {{
      .number_of_channels = {1},
      .sample_formats = {fha::DaiSampleFormat::kPcmSigned},
      .frame_formats = {fha::DaiFrameFormat::WithFrameFormatStandard(
          fha::DaiFrameFormatStandard::kI2S)},
      .frame_rates = {48000},
      .bits_per_slot = {32},
      .bits_per_sample = {16},
  }};

  // values too small
  fha::DaiSupportedFormats fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.number_of_channels({0, 1, 2}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.frame_rates({0, 48000}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_slot({0, 32}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_sample({0, 16}),
  }}));

  // values too large
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.number_of_channels({1, 2, 65}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.frame_rates({48000, 2'000'000'000}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_slot({32, 65}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_sample({16, 33}),
  }}));

  // values out of order
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.number_of_channels({2, 1}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.frame_rates({48000, 44100}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_slot({32, 16}),
  }}));
  fmts = valid;
  EXPECT_FALSE(ValidateDaiFormatSets({{
      fmts.bits_per_sample({16, 8}),
  }}));
}

// Unittest ValidateDaiFormat
TEST(ValidateWarningTest, BadDaiFormat) {
  // empty
  EXPECT_FALSE(ValidateDaiFormat({{}}));

  // each missing
  EXPECT_FALSE(ValidateDaiFormat({{
      // .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));
  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      // .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));

  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      // .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));

  // Missing FrameFormat is impossible since DaiFrameFormat (and thus DaiFormat) has a custom ctor.

  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      // .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));

  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      // .bits_per_slot = 32,
      .bits_per_sample = 16,
  }}));
  EXPECT_FALSE(ValidateDaiFormat({{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      // .bits_per_sample = 16,
  }}));

  // Values too low
  const fha::DaiFormat valid = {{
      .number_of_channels = 2,
      .channels_to_use_bitmask = 0x03,
      .sample_format = fha::DaiSampleFormat::kPcmSigned,
      .frame_format =
          fha::DaiFrameFormat::WithFrameFormatStandard(fha::DaiFrameFormatStandard::kI2S),
      .frame_rate = 48000,
      .bits_per_slot = 32,
      .bits_per_sample = 16,
  }};
  fha::DaiFormat fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.number_of_channels(0)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.channels_to_use_bitmask(0x00)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.frame_rate(0)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_slot(0).bits_per_sample(0)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_sample(0)));

  // values too large
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.number_of_channels(65)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.channels_to_use_bitmask(0x04)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.frame_rate(2'000'000'000)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_slot(kMaxSupportedDaiFormatBitsPerSlot + 1)));
  fmt = valid;
  EXPECT_FALSE(ValidateDaiFormat(fmt.bits_per_sample(33)));
}

// Unittest ValidateCodecFormatInfo
TEST(ValidateWarningTest, BadCodecFormatInfo) {
  // These durations cannot be negative.
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .external_delay = -1,
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_on_delay = -1,
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_off_delay = -1,
  }}));
  // ...that includes INT64_MIN (check for erroneously treating it as unsigned).
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .external_delay = zx::time::infinite_past().get(),
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_on_delay = zx::time::infinite_past().get(),
  }}));
  EXPECT_FALSE(ValidateCodecFormatInfo(fha::CodecFormatInfo{{
      .turn_off_delay = zx::time::infinite_past().get(),
  }}));
}

// signalprocessing functions
//
TEST(ValidateWarningTest, BadElement) {
  // This element has no 'id'.
  EXPECT_FALSE(ValidateElement(kElementNoId));

  // This element has no 'type'.
  EXPECT_FALSE(ValidateElement(kElementNoType));

  // This element has no 'type_specific', but its 'type' requires one.
  EXPECT_FALSE(ValidateElement(kElementNoRequiredTypeSpecific));

  // This element contains a 'type_specific' that does not match its 'type'.
  EXPECT_FALSE(ValidateElement(kElementWrongTypeSpecific));

  // This element contains a 'description' that is an empty string.
  EXPECT_FALSE(ValidateElement(kElementEmptyDescription));

  // Test inconsistencies in certain type_specifics
  // TODO(https://fxbug.dev/42069012): Negative-test ValidateElement
}

TEST(ValidateWarningTest, BadElementList) {
  EXPECT_FALSE(ValidateElements(kEmptyElements));

  // List contains two elements with the same id.
  EXPECT_FALSE(ValidateElements(kElementsDuplicateId));

  // bad Elements: all the ValidateElement negative cases
  EXPECT_FALSE(ValidateElements(kElementsWithNoId));
  EXPECT_FALSE(ValidateElements(kElementsWithNoType));
  EXPECT_FALSE(ValidateElements(kElementsWithNoRequiredTypeSpecific));
  EXPECT_FALSE(ValidateElements(kElementsWithWrongTypeSpecific));
  EXPECT_FALSE(ValidateElements(kElementsWithEmptyDescription));
}

TEST(ValidateWarningTest, BadTopology) {
  // This topology has no 'id'.
  EXPECT_FALSE(ValidateTopology(kTopologyMissingId, MapElements(kElements)));

  // This topology has no 'processing_elements_edge_pairs'.
  EXPECT_FALSE(ValidateTopology(kTopologyMissingEdgePairs, MapElements(kElements)));

  // This topology has an 'processing_elements_edge_pairs' vector that is empty.
  EXPECT_FALSE(ValidateTopology(kTopologyEmptyEdgePairs, MapElements(kElements)));

  // This topology references an element_id that is not included in the element_map.
  EXPECT_FALSE(ValidateTopology(kTopologyUnknownElementId, MapElements(kElements)));

  // This topology includes an edge that connects one element_id to itself.
  EXPECT_FALSE(ValidateTopology(kTopologyEdgePairLoop, MapElements(kElements)));

  // This topology has a terminal (source or destination) element that is not an Endpoint.
  EXPECT_FALSE(ValidateTopology(kTopologyTerminalNotEndpoint, MapElements(kElements)));

  // empty element_map
  EXPECT_FALSE(ValidateTopology(kTopology14, kEmptyElementMap));
}

TEST(ValidateWarningTest, BadTopologyList) {
  EXPECT_FALSE(ValidateTopologies(kEmptyTopologies, MapElements(kElements)));

  // List contains two topologies with the same id.
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithDuplicateId, MapElements(kElements)));

  // There are elements that are not mentioned in at least one of the topologies.
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithoutAllElements, MapElements(kElements)));

  // Topology list with a bad Topology: all the ValidateTopology negative cases
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithMissingId, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithMissingEdgePairs, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithEmptyEdgePairs, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithUnknownElementId, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithLoop, MapElements(kElements)));
  EXPECT_FALSE(ValidateTopologies(kTopologiesWithTerminalNotEndpoint, MapElements(kElements)));

  // empty element_map
  EXPECT_FALSE(ValidateTopologies(kTopologies, kEmptyElementMap));
}

TEST(ValidateWarningTest, ElementStateWithMissingFields) {
  EXPECT_FALSE(ValidateElementState(kElementStateEmpty, kElement1));

  ASSERT_TRUE(ValidateElementState(kElementState1, kElement1));  // Baseline

  // The `started` field is required.
  fuchsia_hardware_audio_signalprocessing::ElementState state_without_started = kElementState1;
  state_without_started.started(std::nullopt);
  EXPECT_FALSE(ValidateElementState(state_without_started, kElement1));

  // For kElement1's ElementType (endpoint), `type_specific` is required.
  fuchsia_hardware_audio_signalprocessing::ElementState state_without_type_specific =
      kElementState1;
  state_without_type_specific.type_specific(std::nullopt);
  EXPECT_FALSE(ValidateElementState(state_without_type_specific, kElement1));
}

// ElementState's type_specific union must match its Element's type.
TEST(ValidateWarningTest, ElementStateWithIncorrectTypeSpecificState) {
  ASSERT_TRUE(ValidateElementState(kElementState1, kElement1));  // Baseline

  // Element is an Endpoint, but the state has an Equalizer type_specific table.
  fuchsia_hardware_audio_signalprocessing::ElementState state_with_incorrect_type_specific =
      kElementState1;
  state_with_incorrect_type_specific.type_specific(
      fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEqualizer(
          {{.band_states = {{{{.id = 0}}}}}}));
  EXPECT_FALSE(ValidateElementState(state_with_incorrect_type_specific, kElement1));
}

// ElementState that violates the capabilities of that element.
TEST(ValidateWarningTest, InconsistentElementState) {
  // According to Element properties it cannot stop, but ElementState says it is stopped.
  EXPECT_FALSE(ValidateElementState(kElementStateStopped, kElementCannotStop));

  // According to Element properties it cannot bypass, but ElementState says it is bypassed.
  EXPECT_FALSE(ValidateElementState(kElementStateBypassed, kElementCannotBypass));

  // More negative tests here that are type-specific.
}

TEST(ValidateWarningTest, ElementStateWithNegativeDurations) {
  ASSERT_TRUE(ValidateElementState(kElementState1, kElement1));  // Baseline

  // `turn_on_delay` is optional, but if present then it cannot be negative.
  fuchsia_hardware_audio_signalprocessing::ElementState state_with_negative_turn_on_delay =
      kElementState1;
  state_with_negative_turn_on_delay.turn_on_delay(ZX_NSEC(-1));
  EXPECT_FALSE(ValidateElementState(state_with_negative_turn_on_delay, kElement1));

  // `turn_off_delay` is optional, but if present then it cannot be negative.
  fuchsia_hardware_audio_signalprocessing::ElementState state_with_negative_turn_off_delay =
      kElementState1;
  state_with_negative_turn_off_delay.turn_on_delay(ZX_NSEC(-1));
  EXPECT_FALSE(ValidateElementState(state_with_negative_turn_off_delay, kElement1));
}

}  // namespace
}  // namespace media_audio

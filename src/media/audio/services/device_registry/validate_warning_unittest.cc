// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <cmath>
#include <cstdint>
#include <optional>
#include <set>
#include <vector>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

// TODO(fxbug.dev/117826): Negative-test ValidateStreamProperties

// TODO(fxbug.dev/117826): Negative-test ValidateSupportedFormats

// TODO(fxbug.dev/117826): Negative-test ValidateGainState
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
                                  // can_mute is missing: CANNOT mute,
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
                                  // plugged is missing
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
                                  // plug_state_time is missing
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

// TODO(fxbug.dev/117826): Negative-test ValidateDeviceInfo

// Negative-test ValidateRingBufferProperties
TEST(ValidateWarningTest, BadRingBufferProperties) {
  // empty
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{}),
            ZX_ERR_INVALID_ARGS);

  // bad external_delay
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .external_delay = -1,
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = 125,
                .driver_transfer_bytes = 128,
            }}),
            ZX_ERR_OUT_OF_RANGE);

  // missing needs_cache_flush_or_invalidate
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .external_delay = 25,
                .turn_on_delay = 125,
                .driver_transfer_bytes = 128,
            }}),
            ZX_ERR_INVALID_ARGS);

  // bad turn_on_delay
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .external_delay = 25,
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = -1,
                .driver_transfer_bytes = 128,
            }}),
            ZX_ERR_OUT_OF_RANGE);

  // missing driver_transfer_bytes
  EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
                .external_delay = 25,
                .needs_cache_flush_or_invalidate = true,
                .turn_on_delay = 125,
            }}),
            ZX_ERR_INVALID_ARGS);

  // // bad driver_transfer_bytes (larger than RB size?)
  // EXPECT_EQ(ValidateRingBufferProperties(fuchsia_hardware_audio::RingBufferProperties{{
  //               .external_delay = 25,
  //               .needs_cache_flush_or_invalidate = true,
  //               .turn_on_delay = 125,
  //               .driver_transfer_bytes = 0xFFFFFFFF,
  //           }}),
  //           ZX_ERR_OUT_OF_RANGE);
}

// Negative-test ValidateRingBufferFormat
TEST(ValidateWarningTest, BadRingBufferFormat) {
  // missing pcm_format
  EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{}), ZX_ERR_INVALID_ARGS);

  // bad value number_of_channels
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
  // EXPECT_EQ(ValidateRingBufferFormat(fuchsia_hardware_audio::Format{{
  //               .pcm_format = fuchsia_hardware_audio::PcmFormat{{
  //                   .number_of_channels = 255,
  //                   .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
  //                   .bytes_per_sample = 2,
  //                   .valid_bits_per_sample = 16,
  //                   .frame_rate = 48000,
  //               }},
  //           }}),
  //           ZX_ERR_OUT_OF_RANGE);

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

// Negative-test ValidateFormatCompatibility
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
        EXPECT_EQ(ValidateFormatCompatibility(sample_size, sample_format), ZX_ERR_INVALID_ARGS);
      }
    }
  }
}

// TODO(fxbug.dev/117826): negative-test ValidateRingBufferVmo
// TEST(ValidateWarningTest, BadRingBufferVmo) {}

// Negative-test ValidateDelayInfo for internal_delay
TEST(ValidateWarningTest, BadInternalDelayInfo) {
  // empty
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{}, std::nullopt),
            ZX_ERR_INVALID_ARGS);

  // missing internal_delay
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                                  .external_delay = 0,
                              }},
                              std::nullopt),
            ZX_ERR_INVALID_ARGS);

  // bad internal_delay
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                                  .internal_delay = -1,
                              }},
                              std::nullopt),
            ZX_ERR_OUT_OF_RANGE);
}

// Negative-test ValidateDelayInfo for external_delay
TEST(ValidateWarningTest, BadExternalDelayInfo) {
  // bad external_delay
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                                  .internal_delay = 0,
                                  .external_delay = -1,
                              }},
                              std::nullopt),
            ZX_ERR_OUT_OF_RANGE);

  // mismatch external_delay (implicit)
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                                  .internal_delay = 0,
                              }},
                              fuchsia_hardware_audio::RingBufferProperties{{
                                  .external_delay = 124,
                                  .needs_cache_flush_or_invalidate = true,
                                  .driver_transfer_bytes = 64,
                              }}),
            ZX_ERR_INVALID_ARGS);

  // mismatch external_delay (explicit)
  EXPECT_EQ(ValidateDelayInfo(fuchsia_hardware_audio::DelayInfo{{
                                  .internal_delay = 0,
                                  .external_delay = 124,
                              }},
                              fuchsia_hardware_audio::RingBufferProperties{{
                                  .external_delay = 0,
                                  .needs_cache_flush_or_invalidate = true,
                                  .driver_transfer_bytes = 64,
                              }}),
            ZX_ERR_INVALID_ARGS);
}

}  // namespace media_audio

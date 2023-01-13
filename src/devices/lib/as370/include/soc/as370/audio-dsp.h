// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_AS370_INCLUDE_SOC_AS370_AUDIO_DSP_H_
#define SRC_DEVICES_LIB_AS370_INCLUDE_SOC_AS370_AUDIO_DSP_H_

#include <zircon/types.h>

#include <memory>
#include <utility>

// #define TESTING_CAPTURE_PDM

// TODO(andresoportus) generalize and place in signal processing library.
class CicFilter {
 public:
  explicit CicFilter() = default;
  virtual ~CicFilter() = default;
  zx_status_t SetInputBitsPerSample(uint32_t input_bits_per_sample) {
    if (input_bits_per_sample != 32 && input_bits_per_sample != 64) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    input_bits_per_sample_ = input_bits_per_sample;
    return ZX_OK;
  }
  virtual uint32_t Filter(uint32_t index, void* input, uint32_t input_size, void* output,
                          uint32_t input_total_channels, uint32_t input_channel,
                          uint32_t output_total_channels,
                          uint32_t output_channel);  // virtual for unit testing.
  uint32_t GetInputToOutputRatio() const { return input_bits_per_sample_ / kOutputBitsPerSample; }

 private:
  static constexpr uint32_t kMaxIndex = 4;
  static constexpr uint32_t kOrder = 5;
  static constexpr uint32_t kOutputBitsPerSample = 16;

  int32_t integrator_state[kMaxIndex][kOrder] = {};
  int32_t differentiator_state[kMaxIndex][kOrder] = {};
  int32_t dc[kMaxIndex] = {};
  uint32_t input_bits_per_sample_ = 64;
};

#endif  // SRC_DEVICES_LIB_AS370_INCLUDE_SOC_AS370_AUDIO_DSP_H_

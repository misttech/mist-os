// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <array>

#include <soc/as370/audio-dsp.h>
#include <zxtest/zxtest.h>

namespace audio {

TEST(CicFilterTest, OnesFilled) {
  CicFilter filter;
  std::array<uint8_t, 0x40> in;
  // every 64 bits in, generate 32 bits that are converted to 16 bit.
  std::array<uint8_t, in.size() / 4> out;
  in.fill(0xff);
  out.fill(0);
  constexpr uint32_t input_channel = 0;
  constexpr uint32_t n_input_channels = 1;
  constexpr uint32_t output_channel = 0;
  constexpr uint32_t n_output_channels = 1;
  filter.Filter(0, in.begin(), in.size(), out.begin(), n_input_channels, input_channel,
                n_output_channels, output_channel);
  // Expected values initially converge to the maximum 16 bits signed integer possible.
  uint16_t expected[] = {0x013e, 0x1ea9, 0x64ff, 0x7f0f, 0x7fef, 0x7fe7, 0x7fdf, 0x7fd7};
  ASSERT_EQ(out.size(), std::size(expected) * sizeof(uint16_t));
  EXPECT_BYTES_EQ(out.begin(), expected, out.size());
}

TEST(CicFilterTest, MultipleChannels) {
  CicFilter filter;
  std::array<uint8_t, 0x40> in;
  // every 64 bits in, generate 32 bits that are converted to 16 bits.
  std::array<uint8_t, in.size() / 4> out;
  in.fill(0xff);
  out.fill(0);
  constexpr uint32_t input_channel = 0;
  constexpr uint32_t n_input_channels = 2;
  constexpr uint32_t output_channel = 1;
  constexpr uint32_t n_output_channels = 2;
  filter.Filter(0, in.begin(), in.size(), out.begin(), n_input_channels, input_channel,
                n_output_channels, output_channel);
  // Non zero values for filtering in output slot 1 only, zeros in slot 2.
  uint16_t expected[] = {0x0000, 0x013e, 0x0000, 0x1ea9, 0x0000, 0x64ff, 0x0000, 0x7f0f};
  ASSERT_EQ(out.size(), std::size(expected) * sizeof(uint16_t));
  EXPECT_BYTES_EQ(out.begin(), expected, out.size());
}

TEST(CicFilterTest, ManyZerosAndOnesEqual) {
  CicFilter filter;
  std::array<uint8_t, 0x4000> in;
  // every 64 bits in, generate 32 bits that are converted to 16 bits.
  std::array<uint8_t, in.size() / 4> out;
  in.fill(0xf0);  // alternate few ones and few zeros to get eventually all 0s.
  out.fill(0);
  constexpr uint32_t input_channel = 0;
  constexpr uint32_t n_input_channels = 2;
  constexpr uint32_t output_channel = 0;
  constexpr uint32_t n_output_channels = 2;
  constexpr size_t kCountEnoughToGetConvergence = 10;
  for (size_t i = 0; i < kCountEnoughToGetConvergence; ++i) {
    filter.Filter(0, in.begin(), in.size(), out.begin(), n_input_channels, input_channel,
                  n_output_channels, output_channel);
  }
  // Expected values converge to zero, we check the last half.
  uint16_t expected[out.size() / 2] = {0};
  EXPECT_BYTES_EQ(out.end() - std::size(expected), expected, std::size(expected));
}

TEST(CicFilterTest, DirectCurrentRemoval) {
  CicFilter filter;
  static std::array<uint8_t, 0x40> in;
  // every 64 bits in, generate 32 bits that are converted to 16 bits.
  std::array<uint8_t, in.size() / 4> out;
  in.fill(0xf8);  // Input with DC, every byte has one more 1 that 0s.
  out.fill(0);
  constexpr uint32_t input_channel = 0;
  constexpr uint32_t n_input_channels = 2;
  constexpr uint32_t output_channel = 0;
  constexpr uint32_t n_output_channels = 2;
  constexpr size_t kCountEnoughToGetDcRemoved = 11'000;
  for (size_t i = 0; i < kCountEnoughToGetDcRemoved; ++i) {
    filter.Filter(0, in.begin(), in.size(), out.begin(), n_input_channels, input_channel,
                  n_output_channels, output_channel);
  }
  // Expected values converge to zero.
  uint16_t expected[out.size()] = {0};
  EXPECT_BYTES_EQ(out.end() - std::size(expected), expected, std::size(expected));
}

}  // namespace audio

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>

#include <ffl/exponential_average.h>
#include <zxtest/zxtest.h>

using ffl::ExponentialAverage;
using ffl::Fixed;
using ffl::FromInteger;
using ffl::FromRatio;

TEST(FuchsiaFixedPoint, ExponentialAverageSingleRate) {
  const Fixed<int32_t, 30> alpha = FromRatio(1, 1);

  Fixed<zx_duration_t, 0> sample = FromInteger(ZX_MSEC(1));
  Fixed<zx_duration_t, 0> expected = FromInteger(ZX_MSEC(1));
  ExponentialAverage avg(sample, alpha);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(2));
  expected = FromInteger(ZX_MSEC(2));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(5));
  expected = FromInteger(ZX_MSEC(5));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(3));
  expected = FromInteger(ZX_MSEC(3));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(1));
  expected = FromInteger(ZX_MSEC(1));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);
}

TEST(FuchsiaFixedPoint, ExponentialAverageDualRate) {
  const Fixed<int32_t, 30> alpha = FromRatio(1, 4);
  const Fixed<int32_t, 30> beta = FromRatio(1, 1);

  Fixed<zx_duration_t, 0> sample = FromInteger(ZX_MSEC(1));
  Fixed<zx_duration_t, 0> expected = FromInteger(ZX_MSEC(1));
  ExponentialAverage avg(sample, alpha, beta);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(2));
  expected = FromInteger(ZX_MSEC(2));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(5));
  expected = FromInteger(ZX_MSEC(5));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(1));
  expected = FromInteger(ZX_MSEC(4));
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);

  sample = FromInteger(ZX_MSEC(1));
  expected = FromInteger(3250000);
  avg.AddSample(sample);
  EXPECT_EQ(avg.value().raw_value(), expected);
}

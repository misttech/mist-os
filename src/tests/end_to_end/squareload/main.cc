// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>

#include <cstdint>

#include <gtest/gtest.h>

#include "src/tests/end_to_end/power/power_utils.h"

namespace squareload {
namespace {
uint64_t totalDurationSec = 60;
uint64_t dutyCycles = 10;
}  // namespace

void squareWaveCPU(uint64_t totalDurationSec, uint64_t dutyCycles) {
  uint64_t intenseDuration = totalDurationSec / (dutyCycles * 2);
  uint64_t idleDuration = intenseDuration;  // 50% duty cycle

  for (uint32_t i = 0; i < dutyCycles; ++i) {
    FX_LOGS(INFO) << "Intense computation phase: CPU utilization at 100% for " << intenseDuration
                  << " seconds";
    power::intenseComputationOnAllCores(intenseDuration);

    FX_LOGS(INFO) << "Idle phase: CPU utilization at 0% for " << idleDuration << " seconds";
    power::idleThread(idleDuration);
  }
}
}  // namespace squareload

TEST(SquareWaveLoadTest, True) {
  FX_LOGS(INFO) << "Starting squareWaveCPU test ...";
  EXPECT_NO_FATAL_FAILURE(
      squareload::squareWaveCPU(squareload::totalDurationSec, squareload::dutyCycles));
  FX_LOGS(INFO) << "Ending squareWaveCPU test!";
}

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>

#include <chrono>
#include <cmath>
#include <cstdint>
#include <thread>
#include <vector>

#include <gtest/gtest.h>

namespace squareload {
  namespace {
    uint64_t totalDurationSec = 60;
    uint64_t dutyCycles = 10;
  }

  void intenseComputation(uint64_t duration) {
    std::chrono::high_resolution_clock::time_point start = std::chrono::high_resolution_clock::now();
    std::chrono::high_resolution_clock::time_point end = start + std::chrono::seconds(duration);
    double x __attribute__((unused)) = 0.0;

    while (std::chrono::high_resolution_clock::now() < end) {
      x += std::sin(0.0001) * std::exp(0.0001);
    }
  }

  void idleCPU(uint64_t duration) {
    std::this_thread::sleep_for(
        std::chrono::seconds(static_cast<std::chrono::seconds::rep>(duration)));
  }

  void intenseComputationOnAllCores(uint64_t duration) {
    const uint32_t numThreads = std::thread::hardware_concurrency();

    std::vector<std::thread> threads;
    for (uint32_t i = 0; i < numThreads; ++i) {
      threads.emplace_back([duration]() { intenseComputation(duration); });
    }

    for (uint32_t i = 0; i < numThreads; ++i) {
      threads[i].join();
    }
  }

  void squareWaveCPU(uint64_t totalDurationSec, uint64_t dutyCycles) {
    uint64_t intenseDuration = totalDurationSec / (dutyCycles * 2);
    uint64_t idleDuration = intenseDuration;  // 50% duty cycle

    for (uint32_t i = 0; i < dutyCycles; ++i) {
      FX_LOGS(INFO) << "Intense computation phase: CPU utilization at 100% for " << intenseDuration
                    << " seconds";
      intenseComputationOnAllCores(intenseDuration);

      FX_LOGS(INFO) << "Idle phase: CPU utilization at 0% for " << idleDuration << " seconds";
      idleCPU(idleDuration);
    }
  }
}

TEST(SquareWaveLoadTest, True) {
  FX_LOGS(INFO) << "Starting squareWaveCPU test ...";
  EXPECT_NO_FATAL_FAILURE(squareload::squareWaveCPU(squareload::totalDurationSec, squareload::dutyCycles));
  FX_LOGS(INFO) << "Ending squareWaveCPU test!";
}

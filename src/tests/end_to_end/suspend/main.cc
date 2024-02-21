// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syslog/cpp/macros.h>

#include <cstdint>
#include <thread>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/tests/end_to_end/power/power_utils.h"

namespace suspend {
namespace {
uint64_t intenseDuration = 100;
uint64_t resumeInterval = 5;
uint64_t cycles = 10;
}  // namespace

void sysfs_suspend() {
  std::thread suspend_thread([]() {
    for (uint32_t i = 0; i < cycles; ++i) {
      FX_LOGS(INFO) << "Intense computation phase: CPU utilization at 100% for " << resumeInterval
                    << " seconds";
      // By idling the thread, we are yielding to the intensive load.
      power::idleThread(resumeInterval);
      FX_LOGS(INFO) << "Suspend phase: CPUs are suspended for 5 seconds";
      ASSERT_TRUE(files::WriteFile("/sys/power/state", "mem"));
    }
  });
  suspend_thread.detach();
  FX_LOGS(INFO) << "Starting the intense computation";
  power::intenseComputationOnAllCores(intenseDuration);
}
}  // namespace suspend

TEST(SuspendCPUTest, True) {
  FX_LOGS(INFO) << "Starting suspendCPU test ...";
  EXPECT_NO_FATAL_FAILURE(suspend::sysfs_suspend());
  FX_LOGS(INFO) << "Ending suspendCPU test!";
}

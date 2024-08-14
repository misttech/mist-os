// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/async/default.h>

#include <src/lib/testing/loop_fixture/real_loop_fixture.h>

#include "gtest/gtest.h"
#include "src/ui/bin/system_monitor/system_monitor.h"

namespace ui_testing {

class SystemMonitorTest : public gtest::RealLoopFixture {};

TEST_F(SystemMonitorTest, GetsCPUData) {
  system_monitor::SystemMonitor systemMonitor;
  systemMonitor.ConnectToArchiveAccessor(true);
  systemMonitor.UpdateRecentDiagnostic();
  ASSERT_TRUE(RunLoopWithTimeoutOrUntil(
      [&systemMonitor]() {
        std::string cpuData = systemMonitor.GetCPUData();
        return !cpuData.empty();
      },
      // This test relies on cobalt_system_metrics to dump cpu data to inspect. This timeout gives
      // it a chance to launch.
      zx::min(1), zx::sec(5)));
}
}  // namespace ui_testing

// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/stall.h>
#include <lib/unittest/unittest.h>

#include <ktl/enforce.h>

class StallAccumulatorTests {
 public:
  // Verifies that non-stalling threads only generate `active` time while running.
  static bool NonStallingTimeTest() {
    BEGIN_TEST;

    StallAccumulator accumulator;

    // Progressing non-stalling time should make only the `active` timer grow.
    accumulator.Update(+1, 0);
    spin(1'000);
    accumulator.Update(-1, 0);
    StallAccumulator::Stats stats1 = accumulator.Flush();
    EXPECT_LE(1'000'000, stats1.total_time_active);
    EXPECT_EQ(0, stats1.total_time_stall_some);
    EXPECT_EQ(0, stats1.total_time_stall_full);

    // Non-progressing non-stalling time should not make any timer grow.
    spin(1'000);
    StallAccumulator::Stats stats2 = accumulator.Flush();
    EXPECT_EQ(0, stats2.total_time_active);
    EXPECT_EQ(0, stats2.total_time_stall_some);
    EXPECT_EQ(0, stats2.total_time_stall_full);

    END_TEST;
  }

  // Verifies that the `some` and `full` stall timers grow with stalling threads.
  static bool StallingTimeTest() {
    BEGIN_TEST;

    StallAccumulator accumulator;

    // Verifies that a single stalling thread makes all the timers grow.
    // Checks: active == some == full >= 1 ms.
    accumulator.Update(0, +1);
    spin(1'000);
    StallAccumulator::Stats stats1 = accumulator.Flush();
    EXPECT_EQ(stats1.total_time_active, stats1.total_time_stall_some);
    EXPECT_EQ(stats1.total_time_stall_some, stats1.total_time_stall_full);
    EXPECT_GE(stats1.total_time_stall_full, 1'000'000);

    // Verifies that adding a second non-stalling thread makes `full` stop growing, but not the
    // others. Checks: active == some >= 1 ms && (some - 1 ms) >= full
    accumulator.Update(+1, 0);
    spin(1'000);
    StallAccumulator::Stats stats2 = accumulator.Flush();
    EXPECT_EQ(stats2.total_time_active, stats2.total_time_stall_some);
    ASSERT_GE(stats2.total_time_stall_some, 1'000'000);
    EXPECT_GE(stats2.total_time_stall_some - 1'000'000, stats2.total_time_stall_full);

    // Cleanup.
    accumulator.Update(-1, -1);

    END_TEST;
  }
};

UNITTEST_START_TESTCASE(stall_tests)
UNITTEST("StallAccumulator - non-stalling time", StallAccumulatorTests::NonStallingTimeTest)
UNITTEST("StallAccumulator - stalling time", StallAccumulatorTests::StallingTimeTest)
UNITTEST_END_TESTCASE(stall_tests, "stall", "pressure stall tests")

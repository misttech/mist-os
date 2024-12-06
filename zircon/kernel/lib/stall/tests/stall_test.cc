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

class StallAggregatorTests {
 public:
  // Verifies that the aggregated stats are the average of per-CPU stats, weighted by `active` time.
  static bool MultiCpuMergeTest() {
    BEGIN_TEST;

    // Simulate a two-CPU system in which the second CPU has 3 times the `active` time of the first
    // one.
    StallAggregator aggregator;
    aggregator.SampleOnce([](StallAggregator::PerCpuStatsCallback callback) {
      callback(StallAccumulator::Stats{
          .total_time_stall_some = 3000,
          .total_time_stall_full = 300,
          .total_time_active = 10000,
      });
      callback(StallAccumulator::Stats{
          .total_time_stall_some = 4000,
          .total_time_stall_full = 400,
          .total_time_active = 30000,
      });
    });
    StallAggregator::Stats stats = aggregator.ReadStats();
    EXPECT_EQ((3000 * 1 + 4000 * 3) / (1 + 3), stats.stalled_time_some);
    EXPECT_EQ((300 * 1 + 400 * 3) / (1 + 3), stats.stalled_time_full);

    END_TEST;
  }

  // Verifies that global stats keep increasing by simulating a system with one CPU, and verifying
  // that the global stats correspond to the running total of values it reports.
  static bool TimersDoNotRestartTest() {
    BEGIN_TEST;

    StallAggregator aggregator;

    aggregator.SampleOnce([](StallAggregator::PerCpuStatsCallback callback) {
      callback(StallAccumulator::Stats{
          .total_time_stall_some = 200,
          .total_time_stall_full = 150,
          .total_time_active = 1000,
      });
    });
    StallAggregator::Stats stats1 = aggregator.ReadStats();
    EXPECT_EQ(200, stats1.stalled_time_some);
    EXPECT_EQ(150, stats1.stalled_time_full);

    aggregator.SampleOnce([](StallAggregator::PerCpuStatsCallback callback) {
      callback(StallAccumulator::Stats{
          .total_time_stall_some = 500,
          .total_time_stall_full = 10,
          .total_time_active = 4000,
      });
    });
    StallAggregator::Stats stats2 = aggregator.ReadStats();
    EXPECT_EQ(200 + 500, stats2.stalled_time_some);
    EXPECT_EQ(150 + 10, stats2.stalled_time_full);

    END_TEST;
  }

  // Verifies that, if the overall `active` time of all CPUs is zero (i.e. if the denominator of the
  // weighted average is zero), the resulting stall timers are simply left unchanged.
  static bool IdleSystemTest() {
    BEGIN_TEST;

    StallAggregator aggregator;

    // Simulate two CPUs, both totally idle.
    aggregator.SampleOnce([](StallAggregator::PerCpuStatsCallback callback) {
      callback(StallAccumulator::Stats{
          .total_time_stall_some = 0,
          .total_time_stall_full = 0,
          .total_time_active = 0,
      });
      callback(StallAccumulator::Stats{
          .total_time_stall_some = 0,
          .total_time_stall_full = 0,
          .total_time_active = 0,
      });
    });
    StallAggregator::Stats stats = aggregator.ReadStats();
    EXPECT_EQ(0, stats.stalled_time_some);
    EXPECT_EQ(0, stats.stalled_time_full);

    END_TEST;
  }
};

UNITTEST_START_TESTCASE(stall_tests)
UNITTEST("StallAccumulator - non-stalling time", StallAccumulatorTests::NonStallingTimeTest)
UNITTEST("StallAccumulator - stalling time", StallAccumulatorTests::StallingTimeTest)
UNITTEST("StallAggregator - timers do not restart", StallAggregatorTests::TimersDoNotRestartTest)
UNITTEST("StallAggregator - merge data from multiple CPUs", StallAggregatorTests::MultiCpuMergeTest)
UNITTEST("StallAggregator - idle system", StallAggregatorTests::IdleSystemTest)
UNITTEST_END_TESTCASE(stall_tests, "stall", "pressure stall tests")

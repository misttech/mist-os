// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/utils/cleanup_until_done.h"

#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/time.h>

#include <gtest/gtest.h>

#include "lib/async/default.h"

namespace utils {

TEST(CleanupUntilDone, CleansUpImmediately) {
  bool cleaned_up = false;

  // Cleanup succeeds the first time, without needing to reschedule.
  CleanupUntilDone cleaner_upper(zx::duration(1'000'000'000), [&cleaned_up]() {
    cleaned_up = true;
    return true;
  });

  cleaner_upper.Cleanup(/*ok_to_run_immediately=*/true);

  EXPECT_TRUE(cleaned_up);
}

TEST(CleanupUntilDone, DoesntCleanUpImmediately) {
  async::TestLoop loop;

  bool cleaned_up = false;

  constexpr zx::duration kFiveMilliseconds = zx::msec(5);
  constexpr zx::duration kTenMilliseconds = zx::msec(10);

  // Cleanup always succeeds the first try.
  CleanupUntilDone cleaner_upper(kTenMilliseconds, [&]() {
    cleaned_up = true;
    return true;
  });

  // `ok_to_run_immediately == false` means that we schedule a task instead of attempting cleanup
  // immediately.
  cleaner_upper.Cleanup(/*ok_to_run_immediately=*/false);
  EXPECT_FALSE(cleaned_up);

  // Task was scheduled to run at 10ms, so waiting for 5ms isn't enough.
  loop.RunFor(kFiveMilliseconds);
  EXPECT_FALSE(cleaned_up);

  // Waiting another 5ms allows the task to run.
  loop.RunFor(kFiveMilliseconds);
  EXPECT_TRUE(cleaned_up);
}

TEST(CleanupUntilDone, SchedulesTasksUntilDone) {
  async::TestLoop loop;

  bool cleaned_up = false;
  size_t cleanup_tries = 0;

  constexpr zx::duration kTenMilliseconds = zx::msec(10);
  constexpr zx::time kTwentyFiveMilliseconds{25'000'000};

  // Simulate a situation where cleanup can't complete until some event occurs at 25 milliseconds.
  // For example, a Vulkan command buffer might finish execution and become eligible for reuse.
  CleanupUntilDone cleaner_upper(kTenMilliseconds, [&]() {
    ++cleanup_tries;
    if (async::Now(async_get_default_dispatcher()) >= kTwentyFiveMilliseconds) {
      cleaned_up = true;
      return true;
    }
    return false;
  });

  // Current time <25ms, so cleanup can't complete.
  cleaner_upper.Cleanup(/*ok_to_run_immediately=*/true);
  EXPECT_FALSE(cleaned_up);
  EXPECT_EQ(1U, cleanup_tries);

  // Current time <25ms, so cleanup can't complete.
  loop.RunFor(kTenMilliseconds);
  EXPECT_FALSE(cleaned_up);
  EXPECT_EQ(2U, cleanup_tries);

  // Current time is 25ms, so cleanup could technically complete.  However, the rescheduled task
  // won't run for another 5ms.
  loop.RunUntil(kTwentyFiveMilliseconds);
  EXPECT_FALSE(cleaned_up);
  EXPECT_EQ(3U, cleanup_tries);

  // Finally, the task runs and finished cleanup.
  loop.RunFor(kTenMilliseconds);
  EXPECT_TRUE(cleaned_up);
  EXPECT_EQ(4U, cleanup_tries);
}

}  // namespace utils

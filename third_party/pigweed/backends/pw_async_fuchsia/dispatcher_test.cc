// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pw_async_fuchsia/dispatcher.h"

#include <gtest/gtest.h>

#include "pw_async_fuchsia/util.h"

#define ASSERT_OK(status) ASSERT_EQ(OkStatus(), status)
#define ASSERT_CANCELLED(status) ASSERT_EQ(Status::Cancelled(), status)

using namespace std::chrono_literals;

namespace pw::async::fuchsia {

TEST(DispatcherFuchsiaTest, TimeConversions) {
  zx::time time{timespec{123, 456}};
  chrono::SystemClock::time_point tp = pw_async_fuchsia::ZxTimeToTimepoint(time);
  EXPECT_EQ(tp.time_since_epoch(), 123s + 456ns);
  EXPECT_EQ(pw_async_fuchsia::TimepointToZxTime(tp), time);
}

TEST(DispatcherFuchsiaTest, Basic) {
  FuchsiaDispatcher dispatcher;

  bool set = false;
  Task task([&set](Context& ctx, Status status) {
    ASSERT_OK(status);
    set = true;
  });
  dispatcher.Post(task);

  dispatcher.RunUntilIdle();
  EXPECT_TRUE(set);
}

TEST(DispatcherFuchsiaTest, DelayedTasks) {
  // This test relies on delays which introduce nondeterminism & slowness, so skip by default.
  GTEST_SKIP();
  FuchsiaDispatcher dispatcher;

  int c = 0;
  Task first([&c](Context& ctx, Status status) {
    ASSERT_OK(status);
    c = c * 10 + 1;
  });
  Task second([&c](Context& ctx, Status status) {
    ASSERT_OK(status);
    c = c * 10 + 2;
  });
  Task third([&c](Context& ctx, Status status) {
    ASSERT_OK(status);
    c = c * 10 + 3;
  });

  dispatcher.PostAfter(third, 20ms);
  dispatcher.PostAfter(first, 5ms);
  dispatcher.PostAfter(second, 10ms);

  dispatcher.RunFor(25ms);
  EXPECT_EQ(c, 123);
}

TEST(DispatcherFuchsiaTest, CancelTask) {
  FuchsiaDispatcher dispatcher;

  Task task([](Context& ctx, Status status) { FAIL(); });
  dispatcher.Post(task);
  EXPECT_TRUE(dispatcher.Cancel(task));

  dispatcher.RunUntilIdle();
}

class DestructionChecker {
 public:
  DestructionChecker(bool* flag) : flag_(flag) {}
  DestructionChecker(DestructionChecker&& other) {
    flag_ = other.flag_;
    other.flag_ = nullptr;
  }
  ~DestructionChecker() {
    if (flag_) {
      *flag_ = true;
    }
  }

 private:
  bool* flag_;
};

TEST(DispatcherFuchsiaTest, HeapAllocatedTasks) {
  FuchsiaDispatcher dispatcher;

  int c = 0;
  for (int i = 0; i < 3; i++) {
    pw_async_fuchsia::Post(&dispatcher, [&c](Context& ctx, Status status) {
      ASSERT_OK(status);
      c++;
    });
  }

  EXPECT_EQ(c, 0);
  dispatcher.RunUntilIdle();
  EXPECT_EQ(c, 3);

  // Test that the lambda is destroyed after being called.
  bool flag = false;
  pw_async_fuchsia::Post(&dispatcher,
                         [checker = DestructionChecker(&flag)](Context& ctx, Status status) {});
  EXPECT_FALSE(flag);
  dispatcher.RunUntilIdle();
  EXPECT_TRUE(flag);
}

TEST(DispatcherFuchsiaTest, ChainedTasks) {
  FuchsiaDispatcher dispatcher;

  int c = 0;

  pw_async_fuchsia::Post(&dispatcher, [&c](Context& ctx, Status status) {
    ASSERT_OK(status);
    c++;
    pw_async_fuchsia::Post(ctx.dispatcher, [&c](Context& ctx, Status status) {
      ASSERT_OK(status);
      c++;
      pw_async_fuchsia::Post(ctx.dispatcher, [&c](Context& ctx, Status status) {
        ASSERT_OK(status);
        c++;
      });
    });
  });

  dispatcher.RunUntilIdle();
  EXPECT_EQ(c, 3);
}

TEST(DispatcherFuchsiaTest, DestroyLoopInsideTask) {
  FuchsiaDispatcher dispatcher;

  int c = 0;
  auto inc_count = [&c](Context& ctx, Status status) {
    ASSERT_CANCELLED(status);
    ++c;
  };

  // These tasks are never executed and cleaned up in RequestStop().
  Task task0(inc_count), task1(inc_count);
  dispatcher.PostAfter(task0, 20s);
  dispatcher.PostAfter(task1, 21s);

  Task stop_task([&c](Context& ctx, Status status) {
    ASSERT_OK(status);
    ++c;
    static_cast<FuchsiaDispatcher*>(ctx.dispatcher)->DestroyLoop();
  });
  dispatcher.Post(stop_task);

  dispatcher.RunUntilIdle();
  EXPECT_EQ(c, 3);
}

TEST(DispatcherFuchsiaTest, TasksCancelledByDispatcherDestructor) {
  int c = 0;
  auto inc_count = [&c](Context& ctx, Status status) {
    ASSERT_CANCELLED(status);
    ++c;
  };
  Task task0(inc_count), task1(inc_count), task2(inc_count);

  {
    FuchsiaDispatcher dispatcher;
    dispatcher.PostAfter(task0, 10s);
    dispatcher.PostAfter(task1, 10s);
    dispatcher.PostAfter(task2, 10s);
  }

  EXPECT_EQ(c, 3);
}

}  // namespace pw::async::fuchsia

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "pw_async/fake_dispatcher_fixture.h"

#include "gtest/gtest.h"
#include "pw_async_fuchsia/dispatcher.h"

#define ASSERT_OK(status) ASSERT_EQ(OkStatus(), status)
#define ASSERT_CANCELLED(status) ASSERT_EQ(Status::Cancelled(), status)

using namespace std::chrono_literals;

namespace pw::async::fuchsia {
namespace {

using FakeDispatcherFuchsiaFixture = async::test::FakeDispatcherFixture;

TEST_F(FakeDispatcherFuchsiaFixture, PostTasks) {
  int c = 0;
  auto inc_count = [&c](Context& /*ctx*/, Status status) {
    ASSERT_OK(status);
    ++c;
  };

  Task task(inc_count);
  dispatcher().Post(task);

  ASSERT_EQ(c, 0);
  RunUntilIdle();
  ASSERT_EQ(c, 1);
}

TEST_F(FakeDispatcherFuchsiaFixture, DelayedTasks) {
  int c = 0;
  pw::async::Task first([&c](pw::async::Context& ctx, Status status) {
    ASSERT_OK(status);
    c = c * 10 + 1;
  });
  pw::async::Task second([&c](pw::async::Context& ctx, Status status) {
    ASSERT_OK(status);
    c = c * 10 + 2;
  });
  pw::async::Task third([&c](pw::async::Context& ctx, Status status) {
    ASSERT_OK(status);
    c = c * 10 + 3;
  });

  dispatcher().PostAfter(third, 20ms);
  dispatcher().PostAfter(first, 5ms);
  dispatcher().PostAfter(second, 10ms);

  RunFor(25ms);
  EXPECT_EQ(c, 123);
}

TEST_F(FakeDispatcherFuchsiaFixture, CancelTask) {
  pw::async::Task task([](pw::async::Context& ctx, Status status) { FAIL(); });
  dispatcher().Post(task);
  EXPECT_TRUE(dispatcher().Cancel(task));

  RunUntilIdle();
}

TEST_F(FakeDispatcherFuchsiaFixture, HeapAllocatedTasks) {
  int c = 0;
  for (int i = 0; i < 3; i++) {
    pw_async_fuchsia::Post(&dispatcher(), [&c](Context& ctx, Status status) {
      ASSERT_OK(status);
      c++;
    });
  }

  RunUntilIdle();
  EXPECT_EQ(c, 3);
}

TEST_F(FakeDispatcherFuchsiaFixture, ChainedTasks) {
  int c = 0;

  pw_async_fuchsia::Post(&dispatcher(), [&c](Context& ctx, Status status) {
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

  RunUntilIdle();
  EXPECT_EQ(c, 3);
}

TEST_F(FakeDispatcherFuchsiaFixture, DestroyLoopInsideTask) {
  int c = 0;
  auto inc_count = [&c](Context& ctx, Status status) {
    ASSERT_CANCELLED(status);
    ++c;
  };

  // These tasks are never executed and cleaned up in DestroyLoop().
  Task task0(inc_count), task1(inc_count);
  dispatcher().PostAfter(task0, 20ms);
  dispatcher().PostAfter(task1, 21ms);

  Task stop_task([&c](Context& ctx, Status status) {
    ASSERT_OK(status);
    ++c;
    static_cast<test::FakeDispatcher*>(ctx.dispatcher)->RequestStop();
    // Stop has been requested; now drive the Dispatcher so it destroys the loop.
    static_cast<test::FakeDispatcher*>(ctx.dispatcher)->RunUntilIdle();
  });
  dispatcher().Post(stop_task);

  RunUntilIdle();
  EXPECT_EQ(c, 3);
}

}  // namespace
}  // namespace pw::async::fuchsia

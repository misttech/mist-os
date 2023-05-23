// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_RUNTIME_TESTING_LOOP_FIXTURE_TEST_LOOP_FIXTURE_H_
#define LIB_DRIVER_RUNTIME_TESTING_LOOP_FIXTURE_TEST_LOOP_FIXTURE_H_

#include <lib/async/cpp/task.h>
#include <lib/driver/testing/cpp/driver_runtime_env.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/testing.h>
#include <lib/sync/cpp/completion.h>

#include <gtest/gtest.h>

#include "sdk/lib/driver/runtime/testing/cpp/dispatcher.h"

namespace gtest {
// An extension of Test class which sets up a driver runtime message loop for
// the test.
//
// Example:
//
//   class FooTest : public ::gtest::DriverTestLoopFixture { /* ... */ };
//
//   TEST_F(FooTest, TestCase) {
//
//     // Initialize an object with the underlying driver runtime dispatcher.
//     Foo foo(driver_dispatcher());
//
//     /* Run a method on foo in the driver dispatcher */
//     RunOnDispatcher([&]() {foo.DoSomething();});
//
//     /* Wait until any posted tasks on the dispatcher are complete. */
//     WaitUntilIdle();
//
//     /* Make assertions about the state of the test case, say about |foo|. */
//   }
class DriverTestLoopFixture : public ::testing::Test {
 public:
  static void WaitUntilIdle() { fdf_testing_wait_until_all_dispatchers_idle(); }

  void SetUp() override { ::testing::Test::SetUp(); }

  void TearDown() override { ::testing::Test::TearDown(); }

  // Posts a task on the driver dispatcher and waits synchronously until it is completed.
  void RunOnDispatcher(fit::closure task) {
    ASSERT_EQ(ZX_OK,
              fdf::RunOnDispatcherSync(dispatcher_.dispatcher(), std::move(task)).status_value());
  }

  const fdf::SynchronizedDispatcher& driver_dispatcher() { return dispatcher_.driver_dispatcher(); }

 private:
  fdf_testing::DriverRuntimeEnv managed_env_;
  fdf::TestSynchronizedDispatcher dispatcher_{fdf::kDispatcherManaged};
};

}  // namespace gtest

#endif  // LIB_DRIVER_RUNTIME_TESTING_LOOP_FIXTURE_TEST_LOOP_FIXTURE_H_

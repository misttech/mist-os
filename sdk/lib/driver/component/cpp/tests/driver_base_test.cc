// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/tests/test_driver.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime_env.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>

#include <gtest/gtest.h>

// This is the recommended setup if you have a driver that doesn't need to make synchronous FIDL
// calls. Everything runs on the main test thread so everything is safe to access directly.
class TestDefaultDispatcher : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.CreateStartArgsAndServe();
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    zx::result result =
        test_environment_.Initialize(std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, result.status_value());

    // Start driver
    zx::result driver =
        fdf_testing::StartDriver<TestDriver>(std::move(start_args->start_args), test_dispatcher_);
    EXPECT_EQ(ZX_OK, driver.status_value());
    driver_ = driver.value();
  }

  void TearDown() override {
    zx::result result = fdf_testing::TeardownDriver(driver_, test_dispatcher_);
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  TestDriver* driver() { return driver_; }

 private:
  // The dispatcher is set to default, and not managed by driver runtime threads.
  // This is used for both the driver and environment.
  fdf::TestSynchronizedDispatcher test_dispatcher_{fdf::kDispatcherDefault};

  // These will use the driver dispatcher from above since it is set as the default.
  fdf_testing::TestNode node_server_{"root"};

  // The default dispatcher is an fdf_dispatcher so we can add driver transport FIDL servers into
  // this environment.
  fdf_testing::TestEnvironment test_environment_;
  TestDriver* driver_;
};

TEST_F(TestDefaultDispatcher, CreateChildNodeAsync) {
  // Safe to touch driver since the dispatcher is set as the default.
  // Dispatcher does not allow sync calls from the driver so we have to use the async version.
  driver()->CreateChildNodeAsync();
  while (!driver()->async_added_child()) {
    fdf_testing_run_until_idle();
  }
}

// If the environment needs to run on a managed driver dispatcher (for example if the driver needs
// to make sync FIDL calls), we need to run the environment on a managed dispatcher while keeping
// the driver on the main thread.
class TestDefaultDispatcherManagedEnv : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    zx::result driver = fdf_testing::StartDriver<TestDriver>(std::move(start_args->start_args),
                                                             test_driver_dispatcher_);
    EXPECT_EQ(ZX_OK, driver.status_value());
    driver_ = driver.value();
  }

  void TearDown() override {
    zx::result result = fdf_testing::TeardownDriver(driver_, test_driver_dispatcher_);
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  TestDriver* driver() { return driver_; }

  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  // This starts up the initial managed thread. It must come before the dispatcher.
  fdf_testing::DriverRuntimeEnv managed_runtime_env_;

  // Driver dispatcher. Started as the current thread's default dispatcher and not ran on the
  // managed runtime thread pool.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherDefault};

  // Env dispatcher. Managed by driver runtime threads because we need to make sync calls into it.
  fdf::TestSynchronizedDispatcher test_env_dispatcher_{fdf::kDispatcherManaged};

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  // The env_dispatcher is an fdf_dispatcher so we can add driver transport FIDL servers into this
  // environment.
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  TestDriver* driver_;
};

TEST_F(TestDefaultDispatcherManagedEnv, CreateChildNodeSync) {
  // Safe to touch the driver from here since the driver_dispatcher is the default.
  // Dispatcher allows sync calls from the driver so we use the sync version.
  driver()->CreateChildNodeSync();
  EXPECT_TRUE(driver()->sync_added_child());
}

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

 private:
  // Driver dispatcher is set to default, and not managed by driver runtime threads.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherDefault};

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

// You don't really gain anything from doing it this way (vs the above |TestDefaultDispatcher|)
// because it's still single threaded (everything runs on the main test thread).
class TestDefaultDispatcherSeparateEnv : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    // We need to use fdf::WaitFor rather than running it directly or using SyncCall.
    // It can't be run directly because the dispatcher is not set as the default.
    // It can't use SyncCall because the dispatcher is not running on the driver runtime managed
    // threads, so it has to run on the main thread (so we can't block).
    zx::result start_args = fdf::WaitFor(
        node_server_.AsyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe).ToFuture());
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    // fdf::WaitFor has the same reasoning as above.
    zx::result init_result =
        fdf::WaitFor(test_environment_
                         .AsyncCall(&fdf_testing::TestEnvironment::Initialize,
                                    std::move(start_args->incoming_directory_server))
                         .ToFuture());
    EXPECT_EQ(ZX_OK, init_result.status_value());

    // Start driver
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

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  // Driver dispatcher is set to default, and not managed by driver runtime threads.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherDefault};

  // Env dispatcher. Not managed by driver runtime threads.
  fdf::TestSynchronizedDispatcher test_env_dispatcher_{{
      .is_default_dispatcher = false,
      .options = {},
      .dispatcher_name = "test-env-dispatcher",
  }};

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  // The env_dispatcher is an fdf_dispatcher so we can add driver transport FIDL servers into this
  // environment.
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  TestDriver* driver_;
};

TEST_F(TestDefaultDispatcherSeparateEnv, CreateChildNodeAsync) {
  // Safe to touch driver since the dispatcher is set as the default.
  // Dispatcher does not allow sync calls from the driver so we have to use the async version.
  driver()->CreateChildNodeAsync();
  while (!driver()->async_added_child()) {
    fdf_testing_run_until_idle();
  }
}

// If the environment needs to run on a driver dispatcher (for example if we want to provide a
// driver transport service in it), and the driver needs to make sync FIDL calls,
// we need to startup the driver runtime environment and create two separate
// TestSynchronizedDispatchers. The one given to the driver will need to have the option
// to allow_sync_calls.
class TestAllowSyncDriverDispatcherSeparateEnv : public ::testing::Test {
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

  // This MUST be accessed from the driver_dispatcher().
  // Short term solution is to wrap this in a dispatcher bound object for safe access.
  // TODO(fxb/123067): Wrap in dispatcher bound.
  // Longer term solution is to pin the driver_dispatcher to the main thread despite having the
  // managed driver runtime environment.
  // TODO(fxb/123068): pinned dispatcher in managed mode.
  TestDriver* driver() { return driver_; }

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  // This starts up the initial managed thread. It must come before the dispatcher.
  fdf_testing::DriverRuntimeEnv managed_runtime_env_;

  // Driver dispatcher, managed by driver runtime threads because it has allow_sync_calls option.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherNoDefaultAllowSync};

  // Env dispatcher. Managed by driver runtime threads because the test_driver_dispatcher has
  // allow_sync_calls option.
  fdf::TestSynchronizedDispatcher test_env_dispatcher_{{
      .is_default_dispatcher = false,
      .options = {},
      .dispatcher_name = "test-env-dispatcher",
  }};

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  // The env_dispatcher is an fdf_dispatcher so we can add driver transport FIDL servers into this
  // environment.
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  TestDriver* driver_;
};

TEST_F(TestAllowSyncDriverDispatcherSeparateEnv, CreateChildNodeSync) {
  // Can only touch driver from the dispatcher since it is not set as the default.
  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher(), [this]() {
                     // Dispatcher allows sync calls from the driver so we use the sync version.
                     driver()->CreateChildNodeSync();
                     EXPECT_TRUE(driver()->sync_added_child());
                   }).status_value());
}

// If the driver needs to make sync calls but the env doesn't need to be on an fdf_dispatcher,
// then it simplifies things a bit. We can create a single driver dispatcher as the default, and
// an async::Loop on a separate thread for the env. This way we can access the driver safely
// from the test and the driver can make sync calls.
class TestDefaultDispatcherSeparateEnvLoop : public ::testing::Test {
 public:
  void SetUp() override {
    // Start a separate thread for the Env dispatcher.
    zx_status_t status = test_env_dispatcher_.StartThread("test-env-dispatcher");
    EXPECT_EQ(ZX_OK, status);

    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::AsyncTestEnvironment::Initialize,
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

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  // Env dispatcher is going to get its own separate thread.
  async::Loop test_env_dispatcher_{&kAsyncLoopConfigNoAttachToCurrentThread};

  // Driver dispatcher is set to default, and not managed by driver runtime threads.
  // Does not need to have allow_sync_calls option since the environment is in a separate thread,
  // the driver can make sync calls into the env still.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherDefault};

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  // Since the env_dispatcher is not an fdf_dispatcher, we can't provide driver transport FIDLs
  // into this environment.
  async_patterns::TestDispatcherBound<fdf_testing::AsyncTestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  TestDriver* driver_;
};

TEST_F(TestDefaultDispatcherSeparateEnvLoop, CreateChildNodeSync) {
  // Dispatcher does not have the allow_sync_calls option, but because the environment is running
  // in a async::Loop thread, we can make blocking calls into there.
  driver()->CreateChildNodeSync();
  EXPECT_TRUE(driver()->sync_added_child());
}

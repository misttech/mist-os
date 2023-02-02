// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/component/cpp/tests/test_driver.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/start_args.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>

#include <gtest/gtest.h>

class TestDefaultDispatcher : public ::testing::Test {
 public:
  void SetUp() override {
    // Driver dispatcher is set to default, and not managed by driver runtime threads.
    zx::result result = test_driver_dispatcher_.StartAsDefault({}, "test-driver-dispatcher");
    EXPECT_EQ(ZX_OK, result.status_value());

    // fdf::Node
    node_server_.emplace(driver_dispatcher(), "root");

    // Create start args
    zx::result start_args = fdf_testing::CreateStartArgs(node_server_.value());
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    test_environment_.emplace(driver_dispatcher());
    result = test_environment_->Initialize(std::move(start_args->incoming_directory_server));
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

    test_environment_.reset();
  }

  TestDriver* driver() { return driver_; }

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }

 private:
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_;
  std::optional<fdf_testing::TestNode> node_server_;
  std::optional<fdf_testing::TestEnvironment> test_environment_;
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

class TestDefaultDispatcherSeparateEnv : public ::testing::Test {
 public:
  void SetUp() override {
    // Driver dispatcher is set to default, and not managed by driver runtime threads.
    zx::result result = test_driver_dispatcher_.StartAsDefault({}, "test-driver-dispatcher");
    EXPECT_EQ(ZX_OK, result.status_value());

    // Env dispatcher. Not managed by driver runtime threads.
    result = test_env_dispatcher_.Start({}, "test-env-dispatcher");
    EXPECT_EQ(ZX_OK, result.status_value());

    // fdf::Node
    node_server_.emplace(env_dispatcher(), "root");

    // Create start args
    zx::result start_args = fdf_testing::CreateStartArgs(node_server_.value());
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    result = fdf::RunOnDispatcherSync(
        env_dispatcher(),
        [this, server = std::move(start_args->incoming_directory_server)]() mutable {
          test_environment_.emplace(env_dispatcher());
          zx::result result = test_environment_->Initialize(std::move(server));
          EXPECT_EQ(ZX_OK, result.status_value());
        });
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

    result = fdf::RunOnDispatcherSync(env_dispatcher(), [this]() { test_environment_.reset(); });
    EXPECT_EQ(ZX_OK, result.status_value());

    result = test_env_dispatcher_.Stop();
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  TestDriver* driver() { return driver_; }

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_;
  fdf::TestSynchronizedDispatcher test_env_dispatcher_;
  std::optional<fdf_testing::TestNode> node_server_;
  std::optional<fdf_testing::TestEnvironment> test_environment_;
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

class TestAllowSyncDriverDispatcherSeparateEnv : public ::testing::Test {
 public:
  void SetUp() override {
    // Startup the driver runtime env because we are using allow sync.
    EXPECT_EQ(ZX_OK, fdf_env_start());

    // Driver dispatcher, managed by driver runtime threads because it has allow sync option.
    zx::result result = test_driver_dispatcher_.Start(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "test-driver-dispatcher");
    EXPECT_EQ(ZX_OK, result.status_value());

    // Env dispatcher. Managed by driver runtime threads because the test_driver_dispatcher has
    // allow sync option.
    result = test_env_dispatcher_.Start({}, "test-env-dispatcher");
    EXPECT_EQ(ZX_OK, result.status_value());

    // fdf::Node
    node_server_.emplace(env_dispatcher(), "root");

    // Create start args
    zx::result start_args = fdf_testing::CreateStartArgs(node_server_.value());
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment
    result = fdf::RunOnDispatcherSync(
        env_dispatcher(),
        [this, server = std::move(start_args->incoming_directory_server)]() mutable {
          test_environment_.emplace(env_dispatcher());
          zx::result result = test_environment_->Initialize(std::move(server));
          EXPECT_EQ(ZX_OK, result.status_value());
        });
    EXPECT_EQ(ZX_OK, result.status_value());

    zx::result driver = fdf_testing::StartDriver<TestDriver>(std::move(start_args->start_args),
                                                             test_driver_dispatcher_);
    EXPECT_EQ(ZX_OK, driver.status_value());
    driver_ = driver.value();
  }

  void TearDown() override {
    zx::result result = fdf_testing::TeardownDriver(driver_, test_driver_dispatcher_);
    EXPECT_EQ(ZX_OK, result.status_value());

    result = fdf::RunOnDispatcherSync(env_dispatcher(), [this]() { test_environment_.reset(); });
    EXPECT_EQ(ZX_OK, result.status_value());

    result = test_env_dispatcher_.Stop();
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  TestDriver* driver() { return driver_; }

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  fdf::TestSynchronizedDispatcher test_env_dispatcher_;
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_;
  std::optional<fdf_testing::TestNode> node_server_;
  std::optional<fdf_testing::TestEnvironment> test_environment_;
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

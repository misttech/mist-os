// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/runtime/testing/runtime/dispatcher.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime_env.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/zx/result.h>

#include <gtest/gtest.h>

#include "sys_driver_cpp/magma_driver_base.h"

namespace {
class FakeTestDriver : public MagmaTestDriverBase {
 public:
  FakeTestDriver(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaTestDriverBase("fake_test_driver", std::move(start_args),
                            std::move(driver_dispatcher)) {}
  zx::result<> MagmaStart() override {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(MagmaDriver::Create());
    if (!magma_driver()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    set_magma_system_device(magma_driver()->CreateDevice(nullptr));
    if (!magma_system_device()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }
};

class FakeDriver : public MagmaProductionDriverBase {
 public:
  FakeDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : MagmaProductionDriverBase("fake_driver", std::move(start_args),
                                  std::move(driver_dispatcher)) {}
  zx::result<> MagmaStart() override {
    std::lock_guard lock(magma_mutex());

    set_magma_driver(MagmaDriver::Create());
    if (!magma_driver()) {
      return zx::error(ZX_ERR_INTERNAL);
    }

    set_magma_system_device(magma_driver()->CreateDevice(nullptr));
    if (!magma_system_device()) {
      return zx::error(ZX_ERR_INTERNAL);
    }
    return zx::ok();
  }
};

// Check that the test driver class can be instantiated (not started).
TEST(MagmaDriver, CreateTestDriver) {
  fdf::TestSynchronizedDispatcher driver_dispatcher{fdf::kDispatcherDefault};
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server{
      driver_dispatcher.dispatcher(), std::in_place, std::string("root")};

  zx::result start_args = node_server.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  EXPECT_EQ(ZX_OK, start_args.status_value());

  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
                     FakeDriver driver{std::move(start_args->start_args),
                                       driver_dispatcher.driver_dispatcher().borrow()};
                   }).status_value());
}

// Check that the driver class can be instantiated (not started).
TEST(MagmaDriver, CreateDriver) {
  fdf::TestSynchronizedDispatcher driver_dispatcher{fdf::kDispatcherDefault};
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server{
      driver_dispatcher.dispatcher(), std::in_place, std::string("root")};

  zx::result start_args = node_server.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  EXPECT_EQ(ZX_OK, start_args.status_value());
  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher.dispatcher(), [&]() {
                     FakeDriver driver{std::move(start_args->start_args),
                                       driver_dispatcher.driver_dispatcher().borrow()};
                   }).status_value());
}

TEST(MagmaDriver, StartTestDriver) {
  fdf::TestSynchronizedDispatcher driver_dispatcher{fdf::kDispatcherNoDefaultAllowSync};

  // This dispatcher is used by the test environment, and hosts the incoming
  // directory.
  fdf::TestSynchronizedDispatcher test_env_dispatcher{{
      .is_default_dispatcher = true,
      .options = {},
      .dispatcher_name = "test-env-dispatcher",
  }};
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server{
      test_env_dispatcher.dispatcher(), std::in_place, std::string("root")};

  zx::result start_args = node_server.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
  EXPECT_EQ(ZX_OK, start_args.status_value());

  ASSERT_TRUE(start_args.is_ok());

  std::optional<fdf_testing::TestEnvironment> test_environment;
  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(test_env_dispatcher.dispatcher(), [&]() {
                     test_environment.emplace();
                     EXPECT_EQ(ZX_OK,
                               test_environment
                                   ->Initialize(std::move(start_args->incoming_directory_server))
                                   .status_value());
                   }).status_value());

  using FakeLifecycle = fdf::Lifecycle<FakeTestDriver>;
  DriverLifecycle lifecycle{.version = DRIVER_LIFECYCLE_VERSION_3,
                            .v1 = {.start = nullptr, .stop = FakeLifecycle::Stop},
                            .v2 = {FakeLifecycle::PrepareStop},
                            .v3 = {FakeLifecycle::Start}};
  auto driver =
      fdf_testing::StartDriver(std::move(start_args->start_args), driver_dispatcher, lifecycle);

  ASSERT_TRUE(driver.is_ok());

  EXPECT_EQ(ZX_OK,
            fdf_testing::TeardownDriver(*driver, driver_dispatcher, lifecycle).status_value());
  EXPECT_EQ(ZX_OK, fdf::RunOnDispatcherSync(test_env_dispatcher.dispatcher(), [&]() {
                     test_environment.reset();
                   }).status_value());
}

}  // namespace

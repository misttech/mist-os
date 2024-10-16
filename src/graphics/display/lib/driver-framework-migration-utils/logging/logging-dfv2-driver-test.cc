// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/syslog/structured_backend/fuchsia_syslog.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/driver-framework-migration-utils/logging/testing/dfv2-driver-with-logging.h"
#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
class DriverLoggingTest : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    node_server_.emplace("root");
    zx::result start_args = node_server_->CreateStartArgsAndServe();
    EXPECT_OK(start_args);

    // Start the test environment
    test_environment_.emplace();
    zx::result result =
        test_environment_->Initialize(std::move(start_args->incoming_directory_server));
    EXPECT_OK(result);

    // Start driver
    zx::result start_result =
        runtime_.RunToCompletion(driver_.Start(std::move(start_args->start_args)));
    EXPECT_OK(start_result);
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_OK(prepare_stop_result);

    test_environment_.reset();
    node_server_.reset();

    runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());
  }

  fdf_testing::internal::DriverUnderTest<testing::Dfv2DriverWithLogging>& driver() {
    return driver_;
  }

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // These will use the foreground dispatcher.
  std::optional<fdf_testing::TestNode> node_server_;
  std::optional<fdf_testing::internal::TestEnvironment> test_environment_;
  fdf_testing::internal::DriverUnderTest<testing::Dfv2DriverWithLogging> driver_;
};

TEST_F(DriverLoggingTest, MinimumLogLevelTrace) {
  driver()->logger().SetSeverity(FUCHSIA_LOG_TRACE);
  EXPECT_TRUE(driver()->LogTrace());
  EXPECT_TRUE(driver()->LogDebug());
  EXPECT_TRUE(driver()->LogInfo());
  EXPECT_TRUE(driver()->LogWarning());
  EXPECT_TRUE(driver()->LogError());
}

TEST_F(DriverLoggingTest, MinimumLogLevelDebug) {
  driver()->logger().SetSeverity(FUCHSIA_LOG_DEBUG);
  EXPECT_FALSE(driver()->LogTrace());
  EXPECT_TRUE(driver()->LogDebug());
  EXPECT_TRUE(driver()->LogInfo());
  EXPECT_TRUE(driver()->LogWarning());
  EXPECT_TRUE(driver()->LogError());
}

TEST_F(DriverLoggingTest, MinimumLogLevelInfo) {
  driver()->logger().SetSeverity(FUCHSIA_LOG_INFO);
  EXPECT_FALSE(driver()->LogTrace());
  EXPECT_FALSE(driver()->LogDebug());
  EXPECT_TRUE(driver()->LogInfo());
  EXPECT_TRUE(driver()->LogWarning());
  EXPECT_TRUE(driver()->LogError());
}

TEST_F(DriverLoggingTest, MinimumLogLevelWarning) {
  driver()->logger().SetSeverity(FUCHSIA_LOG_WARNING);
  EXPECT_FALSE(driver()->LogTrace());
  EXPECT_FALSE(driver()->LogDebug());
  EXPECT_FALSE(driver()->LogInfo());
  EXPECT_TRUE(driver()->LogWarning());
  EXPECT_TRUE(driver()->LogError());
}

TEST_F(DriverLoggingTest, MinimumLogLevelError) {
  driver()->logger().SetSeverity(FUCHSIA_LOG_ERROR);
  EXPECT_FALSE(driver()->LogTrace());
  EXPECT_FALSE(driver()->LogDebug());
  EXPECT_FALSE(driver()->LogInfo());
  EXPECT_FALSE(driver()->LogWarning());
  EXPECT_TRUE(driver()->LogError());
}
}  // namespace

}  // namespace display

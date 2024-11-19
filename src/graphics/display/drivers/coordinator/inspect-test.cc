// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/default.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/reader.h>

#include <gtest/gtest.h>

#include "lib/fpromise/single_threaded_executor.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"

namespace display_coordinator {

namespace {

struct EngineDriverClientAndServer {
  static EngineDriverClientAndServer Create();

  std::unique_ptr<EngineDriverClient> engine_driver_client;
  fdf::ServerEnd<fuchsia_hardware_display_engine::Engine> engine_server;
};

// static
EngineDriverClientAndServer EngineDriverClientAndServer::Create() {
  auto [engine_client, engine_server] =
      fdf::Endpoints<fuchsia_hardware_display_engine::Engine>::Create();
  return {
      .engine_driver_client = std::make_unique<EngineDriverClient>(std::move(engine_client)),
      .engine_server = std::move(engine_server),
  };
}

class InspectTest : public ::testing::Test {
 public:
  InspectTest()
      : engine_driver_client_and_server_(EngineDriverClientAndServer::Create()),
        inspector_(),
        controller_(std::move(engine_driver_client_and_server_.engine_driver_client),
                    /*dispatcher=*/driver_runtime_.StartBackgroundDispatcher(), inspector_) {}

  void SetUp() override {
    fpromise::result<inspect::Hierarchy> hierarchy_maybe =
        fpromise::run_single_threaded(inspect::ReadFromInspector(inspector_));
    ASSERT_TRUE(hierarchy_maybe.is_ok());

    hierarchy_ = std::move(hierarchy_maybe.value());
  }

 protected:
  // `logger_` must outlive `driver_runtime_` to allow for any
  // logging in driver de-initialization code.
  fdf_testing::ScopedGlobalLogger logger_;
  fdf_testing::DriverRuntime driver_runtime_;

  EngineDriverClientAndServer engine_driver_client_and_server_;
  inspect::Inspector inspector_;
  Controller controller_;
  inspect::Hierarchy hierarchy_;
};

TEST_F(InspectTest, ApplyConfigHierarchy) {
  const inspect::Hierarchy* display = hierarchy_.GetByPath({"display"});
  ASSERT_NE(display, nullptr);
  const inspect::NodeValue& display_node = display->node();

  const inspect::UintPropertyValue* last_valid_apply_config_timestamp_ns =
      display_node.get_property<inspect::UintPropertyValue>("last_valid_apply_config_timestamp_ns");
  ASSERT_NE(last_valid_apply_config_timestamp_ns, nullptr);
  const inspect::UintPropertyValue* last_valid_apply_config_interval_ns =
      display_node.get_property<inspect::UintPropertyValue>("last_valid_apply_config_interval_ns");
  ASSERT_NE(last_valid_apply_config_interval_ns, nullptr);
  const inspect::UintPropertyValue* last_valid_apply_config_stamp =
      display_node.get_property<inspect::UintPropertyValue>("last_valid_apply_config_stamp");
  ASSERT_NE(last_valid_apply_config_stamp, nullptr);
}

TEST_F(InspectTest, VsyncMonitorHierarchy) {
  const inspect::Hierarchy* vsync_monitor = hierarchy_.GetByPath({"display", "vsync_monitor"});
  ASSERT_NE(vsync_monitor, nullptr);
  const inspect::NodeValue& vsync_monitor_node = vsync_monitor->node();

  const inspect::UintPropertyValue* last_vsync_timestamp_ns =
      vsync_monitor_node.get_property<inspect::UintPropertyValue>("last_vsync_timestamp_ns");
  ASSERT_NE(last_vsync_timestamp_ns, nullptr);
  const inspect::UintPropertyValue* last_vsync_interval_ns =
      vsync_monitor_node.get_property<inspect::UintPropertyValue>("last_vsync_interval_ns");
  ASSERT_NE(last_vsync_interval_ns, nullptr);
  const inspect::UintPropertyValue* last_vsync_config_stamp =
      vsync_monitor_node.get_property<inspect::UintPropertyValue>("last_vsync_config_stamp");
  ASSERT_NE(last_vsync_config_stamp, nullptr);

  const inspect::UintPropertyValue* vsync_stalls =
      vsync_monitor_node.get_property<inspect::UintPropertyValue>("vsync_stalls");
  ASSERT_NE(vsync_stalls, nullptr);
}

}  // namespace

}  // namespace display_coordinator

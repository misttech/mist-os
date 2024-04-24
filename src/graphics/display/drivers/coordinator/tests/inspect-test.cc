// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/default.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/inspect/cpp/reader.h>

#include <gtest/gtest.h>

#include "lib/fpromise/single_threaded_executor.h"
#include "src/graphics/display/drivers/coordinator/controller.h"

namespace display {

namespace {

class InspectTest : public ::testing::Test {
 public:
  InspectTest() : inspector_(), controller_(nullptr, inspector_) {}

  void SetUp() override {
    fpromise::result<inspect::Hierarchy> hierarchy_maybe =
        fpromise::run_single_threaded(inspect::ReadFromInspector(inspector_));
    ASSERT_TRUE(hierarchy_maybe.is_ok());

    hierarchy_ = std::move(hierarchy_maybe.value());
  }

 protected:
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

}  // namespace display

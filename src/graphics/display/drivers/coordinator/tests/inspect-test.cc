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

TEST(InspectTest, InspectHierarchy) {
  inspect::Inspector inspector;
  Controller controller(nullptr, inspector);

  fpromise::result<inspect::Hierarchy> hierarchy_maybe =
      fpromise::run_single_threaded(inspect::ReadFromInspector(inspector));
  ASSERT_TRUE(hierarchy_maybe.is_ok());
  const inspect::Hierarchy& hierarchy = hierarchy_maybe.value();
  const inspect::Hierarchy* display = hierarchy.GetByPath({"display"});
  ASSERT_NE(display, nullptr);

  const inspect::NodeValue& node = display->node();
  const inspect::UintPropertyValue* last_vsync_timestamp_ns =
      node.get_property<inspect::UintPropertyValue>("last_vsync_timestamp_ns");
  ASSERT_NE(last_vsync_timestamp_ns, nullptr);
  const inspect::UintPropertyValue* last_vsync_interval_ns =
      node.get_property<inspect::UintPropertyValue>("last_vsync_interval_ns");
  ASSERT_NE(last_vsync_interval_ns, nullptr);
  const inspect::UintPropertyValue* last_vsync_config_stamp =
      node.get_property<inspect::UintPropertyValue>("last_vsync_config_stamp");
  ASSERT_NE(last_vsync_config_stamp, nullptr);

  const inspect::UintPropertyValue* last_valid_apply_config_timestamp_ns =
      node.get_property<inspect::UintPropertyValue>("last_valid_apply_config_timestamp_ns");
  ASSERT_NE(last_valid_apply_config_timestamp_ns, nullptr);
  const inspect::UintPropertyValue* last_valid_apply_config_interval_ns =
      node.get_property<inspect::UintPropertyValue>("last_valid_apply_config_interval_ns");
  ASSERT_NE(last_valid_apply_config_interval_ns, nullptr);
  const inspect::UintPropertyValue* last_valid_apply_config_stamp =
      node.get_property<inspect::UintPropertyValue>("last_valid_apply_config_stamp");
  ASSERT_NE(last_valid_apply_config_stamp, nullptr);

  const inspect::UintPropertyValue* vsync_stalls =
      node.get_property<inspect::UintPropertyValue>("vsync_stalls");
  ASSERT_NE(vsync_stalls, nullptr);
}

}  // namespace

}  // namespace display

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../display-detect-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <bind/fuchsia/display/cpp/bind.h>
#include <gtest/gtest.h>

namespace display_detect_visitor_dt {

class DisplayDetectVisitorTester
    : public fdf_devicetree::testing::VisitorTestHelper<DisplayDetectVisitor> {
 public:
  explicit DisplayDetectVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<DisplayDetectVisitor>(
            dtb_path, "DisplayDetectVisitorTest") {}
};

TEST(DisplayDetectVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<DisplayDetectVisitorTester>("/pkg/test-data/display-detect.dtb");
  DisplayDetectVisitorTester* display_detect_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, display_detect_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(display_detect_visitor_tester->DoPublish().is_ok());

  auto hdmi_node = display_detect_visitor_tester->GetNodes("hdmi-display");
  EXPECT_EQ(hdmi_node.size(), 1u);

  auto hdmi_composite_spec = display_detect_visitor_tester->GetCompositeNodeSpecs("hdmi-display");
  EXPECT_EQ(hdmi_composite_spec.size(), 1u);

  ASSERT_TRUE(hdmi_composite_spec[0].parents2().has_value());
  ASSERT_EQ(2lu, hdmi_composite_spec[0].parents2()->size());

  // 1st parent is pdev. Skip that.
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {{fdf::MakeAcceptBindRule2(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_HDMI)}},
      (*hdmi_composite_spec[0].parents2())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {{fdf::MakeProperty2(bind_fuchsia_display::OUTPUT, bind_fuchsia_display::OUTPUT_HDMI)}},
      (*hdmi_composite_spec[0].parents2())[1].properties(), false));
}

}  // namespace display_detect_visitor_dt

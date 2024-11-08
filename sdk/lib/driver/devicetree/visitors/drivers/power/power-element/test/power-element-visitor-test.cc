// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../power-element-visitor.h"

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <gtest/gtest.h>

#include "fidl/fuchsia.hardware.power/cpp/common_types.h"

namespace power_element_visitor_dt {

class PowerElementVisitorTester
    : public fdf_devicetree::testing::VisitorTestHelper<PowerElementVisitor> {
 public:
  PowerElementVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<PowerElementVisitor>(
            dtb_path, "PowerElementVisitorTest") {}
};

TEST(PowerElementVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<PowerElementVisitorTester>("/pkg/test-data/power-element.dtb");
  PowerElementVisitorTester* power_element_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, power_element_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(power_element_visitor_tester->DoPublish().is_ok());

  auto node_count = power_element_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = power_element_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("bluetooth") != std::string::npos) {
      node_tested_count++;
      ASSERT_TRUE(node.power_config());
      ASSERT_EQ(node.power_config()->size(), 1u);

      ASSERT_TRUE((*node.power_config())[0].element());
      auto element = *(*node.power_config())[0].element();
      EXPECT_EQ(element.name(), "wake-on-interrupt");

      ASSERT_TRUE(element.levels());
      EXPECT_EQ(element.levels()->size(), 3u);
      uint32_t levels_tested = 0;
      for (auto& level : *element.levels()) {
        if (level.name() == "off") {
          levels_tested++;
          EXPECT_EQ(level.level(), 0u);
          ASSERT_TRUE(level.transitions());
          EXPECT_EQ(level.transitions()->size(), 1u);
          EXPECT_EQ(level.transitions()->at(0).target_level(), 1u);
          EXPECT_EQ(level.transitions()->at(0).latency_us(), 1000u);
        }
        if (level.name() == "handling") {
          levels_tested++;
          EXPECT_EQ(level.level(), 1u);
        }
        if (level.name() == "on") {
          levels_tested++;
          EXPECT_EQ(level.level(), 2u);
        }
      }
      EXPECT_EQ(levels_tested, 3u);

      ASSERT_TRUE((*node.power_config())[0].dependencies());
      auto dependencies = (*node.power_config())[0].dependencies();
      EXPECT_EQ(dependencies->size(), 4u);
      uint32_t dependencies_tested = 0;
      for (auto& dependency : *dependencies) {
        EXPECT_EQ(dependency.child(), "wake-on-interrupt");
        ASSERT_TRUE(dependency.level_deps());
        EXPECT_EQ(dependency.level_deps()->size(), 1u);

        if (dependency.parent()->sag().has_value()) {
          if (dependency.parent()->sag().value() ==
              fuchsia_hardware_power::SagElement::kExecutionState) {
            dependencies_tested++;
            if (dependency.level_deps()->at(0).parent_level() == 2u) {
              EXPECT_EQ(dependency.level_deps()->at(0).child_level(), 2u);
              EXPECT_EQ(dependency.strength(),
                        static_cast<fuchsia_hardware_power::RequirementType>(1u));
            } else {
              EXPECT_EQ(dependency.level_deps()->at(0).child_level(), 1u);
              EXPECT_EQ(dependency.level_deps()->at(0).parent_level(), 1u);
              EXPECT_EQ(dependency.strength(),
                        static_cast<fuchsia_hardware_power::RequirementType>(2u));
            }
          }
        }

        if (dependency.parent()->cpu_control().has_value()) {
          dependencies_tested++;
          EXPECT_EQ(dependency.level_deps()->at(0).child_level(), 1u);
          EXPECT_EQ(dependency.level_deps()->at(0).parent_level(), 1u);
          EXPECT_EQ(dependency.strength(),
                    static_cast<fuchsia_hardware_power::RequirementType>(1u));
        }

        if (dependency.parent()->instance_name().has_value()) {
          dependencies_tested++;
          EXPECT_EQ(dependency.parent()->instance_name().value(), "rail-1");
          EXPECT_EQ(dependency.level_deps()->at(0).child_level(), 2u);
          EXPECT_EQ(dependency.level_deps()->at(0).parent_level(), 0u);
          EXPECT_EQ(dependency.strength(),
                    static_cast<fuchsia_hardware_power::RequirementType>(2u));
        }
      }
      EXPECT_EQ(dependencies_tested, 4u);
    }

    if (node.name()->find("power-controller") != std::string::npos) {
      node_tested_count++;
      ASSERT_TRUE(node.power_config());
      ASSERT_EQ(node.power_config()->size(), 1u);

      ASSERT_TRUE((*node.power_config())[0].element());
      auto element = *(*node.power_config())[0].element();
      EXPECT_EQ(element.name(), "rail-1");

      ASSERT_TRUE(element.levels());
      EXPECT_EQ(element.levels()->size(), 1u);
      uint32_t levels_tested = 0;
      for (auto& level : *element.levels()) {
        if (level.name() == "rail-on") {
          levels_tested++;
          EXPECT_EQ(level.level(), 0u);
        }
      }
      EXPECT_EQ(levels_tested, 1u);

      ASSERT_FALSE((*node.power_config())[0].dependencies());
    }
  }

  ASSERT_EQ(node_tested_count, 2u);
}

}  // namespace power_element_visitor_dt

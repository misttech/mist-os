// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec_v2.h"

#include "src/devices/bin/driver_manager/tests/driver_manager_test_base.h"

class CompositeNodeSpecV2Test : public DriverManagerTestBase {
 public:
  void SetUp() override {
    DriverManagerTestBase::SetUp();
    arena_ = std::make_unique<fidl::Arena<512>>();
  }

  driver_manager::NodeManager* GetNodeManager() override { return &node_manager; }

  driver_manager::CompositeNodeSpecV2 CreateCompositeNodeSpec(std::string name, size_t size) {
    std::vector<fuchsia_driver_framework::ParentSpec> parents(size);
    return driver_manager::CompositeNodeSpecV2(
        driver_manager::CompositeNodeSpecCreateInfo{
            .name = std::move(name),
            .parents = std::move(parents),
        },
        dispatcher(), &node_manager);
  }

  zx::result<std::optional<driver_manager::DeviceOrNode>> MatchAndBindParentSpec(
      driver_manager::CompositeNodeSpecV2& spec, std::weak_ptr<driver_manager::Node> parent_node,
      std::vector<std::string> parent_names, uint32_t node_index, uint32_t primary_index = 0) {
    fuchsia_driver_framework::CompositeParent matched_parent({
        .composite = fuchsia_driver_framework::CompositeInfo{{
            .spec = fuchsia_driver_framework::CompositeNodeSpec{{
                .name = spec.name(),
                .parents = std::vector<fuchsia_driver_framework::ParentSpec>(parent_names.size()),
            }},
            .matched_driver = fuchsia_driver_framework::CompositeDriverMatch{{
                .composite_driver = fuchsia_driver_framework::CompositeDriverInfo{{
                    .composite_name = "test-composite",
                    .driver_info = fuchsia_driver_framework::DriverInfo{{
                        .url = "fuchsia-boot:///#meta/composite-driver.cm",
                        .colocate = true,
                    }},
                }},
                .parent_names = parent_names,
                .primary_parent_index = primary_index,
            }},
        }},
        .index = node_index,
    });

    return spec.BindParent(fidl::ToWire(*arena_, matched_parent), parent_node);
  }

  void VerifyCompositeNode(std::weak_ptr<driver_manager::Node> composite_node,
                           std::vector<std::string> expected_parents, uint32_t primary_index) {
    auto composite_node_ptr = composite_node.lock();
    ASSERT_TRUE(composite_node_ptr);
    ASSERT_EQ(expected_parents.size(), composite_node_ptr->parents().size());
    for (size_t i = 0; i < expected_parents.size(); i++) {
      ASSERT_EQ(expected_parents[i], composite_node_ptr->parents()[i].lock()->name());
    }
    ASSERT_EQ(expected_parents[primary_index], composite_node_ptr->GetPrimaryParent()->name());
  }

  TestNodeManagerBase node_manager;

 private:
  std::unique_ptr<fidl::Arena<512>> arena_;
};

TEST_F(CompositeNodeSpecV2Test, SpecBind) {
  auto spec = CreateCompositeNodeSpec("spec", 2);

  // Bind the first node.
  std::shared_ptr parent_1 = CreateNode("spec_parent_1");
  auto result = MatchAndBindParentSpec(spec, parent_1, {"node-0", "node-1"}, 0);
  ASSERT_TRUE(result.is_ok());
  ASSERT_FALSE(result.value());

  // Bind the second node.
  std::shared_ptr parent_2 = CreateNode("spec_parent_2");
  result = MatchAndBindParentSpec(spec, parent_2, {"node-0", "node-1"}, 1);
  ASSERT_TRUE(result.is_ok());
  ASSERT_TRUE(result.value());

  // Verify the parents and primary node.
  auto composite_node = std::get<std::weak_ptr<driver_manager::Node>>(result.value().value());
  VerifyCompositeNode(composite_node, {"spec_parent_1", "spec_parent_2"}, 0);
}

TEST_F(CompositeNodeSpecV2Test, RemoveWithCompositeNode) {
  auto spec = CreateCompositeNodeSpec("spec", 2);

  // Bind the first node.
  std::shared_ptr parent_1 = CreateNode("spec_parent_1");
  auto result = MatchAndBindParentSpec(spec, parent_1, {"node-0", "node-1"}, 0);
  ASSERT_TRUE(result.is_ok());
  ASSERT_FALSE(result.value());

  ASSERT_TRUE(spec.has_parent_set_collector_for_testing());

  // Bind the second node.
  std::shared_ptr parent_2 = CreateNode("spec_parent_2");
  result = MatchAndBindParentSpec(spec, parent_2, {"node-0", "node-1"}, 1);
  ASSERT_TRUE(result.is_ok());
  ASSERT_TRUE(result.value());

  // Verify the parents and primary node.
  auto composite_node = spec.completed_composite_node();
  ASSERT_TRUE(composite_node.has_value());
  auto composite_node_ptr = composite_node->lock();
  VerifyCompositeNode(composite_node.value(), {"spec_parent_1", "spec_parent_2"}, 0);

  // Invoke remove.
  spec.Remove([](zx::result<> result) {});
  ASSERT_EQ(driver_manager::ShutdownIntent::kRebindComposite,
            composite_node_ptr->shutdown_intent());
  ASSERT_FALSE(spec.completed_composite_node().has_value());
  ASSERT_FALSE(spec.has_parent_set_collector_for_testing());
}

TEST_F(CompositeNodeSpecV2Test, RemoveWithNoCompositeNode) {
  auto spec = CreateCompositeNodeSpec("spec", 2);

  // Bind the second node.
  std::shared_ptr parent_2 = CreateNode("spec_parent_2");
  auto result = MatchAndBindParentSpec(spec, parent_2, {"node-0", "node-1"}, 1);
  ASSERT_TRUE(result.is_ok());
  ASSERT_FALSE(result.value());

  ASSERT_TRUE(spec.has_parent_set_collector_for_testing());
  ASSERT_FALSE(spec.completed_composite_node().has_value());

  // Invoke remove.
  spec.Remove([](zx::result<> result) {});
  ASSERT_FALSE(spec.completed_composite_node().has_value());
  ASSERT_FALSE(spec.has_parent_set_collector_for_testing());
}

// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/composite_node_spec_v2.h"

#include "src/devices/bin/driver_manager/v2/node.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

// TODO(fxb/132293): Move FakeNodeManager and common node construction code into a separate class.

class FakeNodeManager : public dfv2::NodeManager {
 public:
  void Bind(dfv2::Node& node, std::shared_ptr<dfv2::BindResultTracker> result_tracker) override {}

  zx::result<dfv2::DriverHost*> CreateDriverHost() override { return zx::ok(nullptr); }

  void DestroyDriverComponent(
      dfv2::Node& node,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_component::Realm::DestroyChild>& result)>
          callback) override {}
};

class CompositeNodeSpecV2Test : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    TestLoopFixture::SetUp();

    arena_ = std::make_unique<fidl::Arena<512>>();

    devfs_.emplace(root_devnode_);
    root_ = CreateNode("root");
    root_->AddToDevfsForTesting(root_devnode_.value());
  }

  dfv2::CompositeNodeSpecV2 CreateCompositeNodeSpec(std::string name, size_t size) {
    return dfv2::CompositeNodeSpecV2(
        CompositeNodeSpecCreateInfo{
            .name = name,
            .size = size,
        },
        dispatcher(), &node_manager);
  }

  std::shared_ptr<dfv2::Node> CreateNode(const char* name) {
    std::shared_ptr new_node =
        std::make_shared<dfv2::Node>(name, std::vector<std::weak_ptr<dfv2::Node>>(), &node_manager,
                                     dispatcher(), inspect_.CreateDevice(name, zx::vmo(), 0));
    new_node->AddToDevfsForTesting(root_devnode_.value());
    return new_node;
  }

  zx::result<std::optional<DeviceOrNode>> MatchAndBindParentSpec(
      dfv2::CompositeNodeSpecV2& spec, std::weak_ptr<dfv2::Node> parent_node,
      std::vector<std::string> parent_names, uint32_t node_index, uint32_t primary_index = 0) {
    auto matched_composite = fuchsia_driver_index::MatchedCompositeInfo(
        {.composite_name = "test-composite",
         .driver_info = fuchsia_driver_index::MatchedDriverInfo(
             {.url = "fuchsia-boot:///#meta/composite-driver.cm", .colocate = true})});
    auto matched_parent = fuchsia_driver_index::MatchedCompositeNodeSpecInfo({
        .name = spec.name(),
        .node_index = node_index,
        .composite = matched_composite,
        .num_nodes = parent_names.size(),
        .node_names = parent_names,
        .primary_index = primary_index,
    });

    return spec.BindParent(fidl::ToWire(*arena_, matched_parent), parent_node);
  }

  void VerifyCompositeNode(std::weak_ptr<dfv2::Node> composite_node,
                           std::vector<std::string> expected_parents, uint32_t primary_index) {
    auto composite_node_ptr = composite_node.lock();
    ASSERT_TRUE(composite_node_ptr);
    ASSERT_EQ(expected_parents.size(), composite_node_ptr->parents().size());
    for (size_t i = 0; i < expected_parents.size(); i++) {
      ASSERT_EQ(expected_parents[i], composite_node_ptr->parents()[i].lock()->name());
    }
    ASSERT_EQ(expected_parents[primary_index], composite_node_ptr->GetPrimaryParent()->name());
  }

  FakeNodeManager node_manager;

 private:
  InspectManager inspect_{dispatcher()};

  std::shared_ptr<dfv2::Node> root_;
  std::optional<Devnode> root_devnode_;
  std::optional<Devfs> devfs_;

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
  auto composite_node = std::get<std::weak_ptr<dfv2::Node>>(result.value().value());
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
  ASSERT_EQ(dfv2::ShutdownIntent::kRebindComposite, composite_node_ptr->shutdown_intent());
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

// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fit/defer.h>

#include <zxtest/zxtest.h>

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"
#include "src/devices/bin/driver_manager/v2/node.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace fdi = fuchsia_driver_index;

struct DeviceV1Wrapper {};

namespace {

fdi::MatchedCompositeNodeSpecInfo MakeCompositeNodeSpecInfo(std::string spec_name, uint32_t index,
                                                            std::vector<std::string> specs) {
  fdi::MatchedCompositeInfo composite{{
      .composite_name = "test_composite",
      .driver_info = fdi::MatchedDriverInfo{},
  }};

  return fdi::MatchedCompositeNodeSpecInfo{{
      .name = spec_name,
      .node_index = index,
      .composite = composite,
      .num_nodes = specs.size(),
      .node_names = specs,
  }};
}

}  // namespace

class FakeCompositeNodeSpec : public CompositeNodeSpec {
 public:
  explicit FakeCompositeNodeSpec(CompositeNodeSpecCreateInfo create_info)
      : CompositeNodeSpec(std::move(create_info)) {}

  zx::result<std::optional<DeviceOrNode>> BindParentImpl(
      fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
      const DeviceOrNode& device_or_node) override {
    return zx::ok(std::shared_ptr<DeviceV1Wrapper>(new DeviceV1Wrapper{}));
  }

  fuchsia_driver_development::wire::CompositeInfo GetCompositeInfo(
      fidl::AnyArena& arena) const override {
    return fuchsia_driver_development::wire::CompositeInfo::Builder(arena).Build();
  }
};

class FakeDeviceManagerBridge : public CompositeManagerBridge {
 public:
  // CompositeManagerBridge:
  void BindNodesForCompositeNodeSpec() override {}
  void AddSpecToDriverIndex(fdf::wire::CompositeNodeSpec spec,
                            AddToIndexCallback callback) override {
    auto iter = spec_matches_.find(std::string(spec.name().get()));
    zx::result<fdi::DriverIndexAddCompositeNodeSpecResponse> result;
    if (iter == spec_matches_.end()) {
      result = zx::error(ZX_ERR_NOT_FOUND);
    } else {
      auto composite = iter->second.composite();
      auto names = iter->second.node_names();
      ZX_ASSERT(composite.has_value());
      ZX_ASSERT(names.has_value());
      result =
          zx::ok(fdi::DriverIndexAddCompositeNodeSpecResponse(composite.value(), names.value()));
    }
    auto defer =
        fit::defer([callback = std::move(callback), result]() mutable { callback(result); });
  }

  void AddSpecMatch(std::string_view name, fdi::MatchedCompositeNodeSpecInfo match) {
    spec_matches_[std::string(name)] = std::move(match);
  }

 private:
  // Stores matches for each composite node spec name, that get returned to the
  // AddToIndexCallback that is given in AddSpecToDriverIndex.
  std::unordered_map<std::string, fdi::MatchedCompositeNodeSpecInfo> spec_matches_;
};

class CompositeNodeSpecManagerTest : public zxtest::Test {
 public:
  void SetUp() override {
    composite_node_spec_manager_ = std::make_unique<CompositeNodeSpecManager>(&bridge_);
  }

  fit::result<fuchsia_driver_framework::CompositeNodeSpecError> AddSpec(
      fuchsia_driver_framework::wire::CompositeNodeSpec fidl_spec) {
    auto spec = std::make_unique<FakeCompositeNodeSpec>(CompositeNodeSpecCreateInfo{
        .name = std::string(fidl_spec.name().get()),
        .size = fidl_spec.parents().count(),
    });
    return composite_node_spec_manager_->AddSpec(fidl_spec, std::move(spec));
  }

  std::shared_ptr<dfv2::Node> CreateNode(const char* name) {
    return std::make_shared<dfv2::Node>("node", std::vector<dfv2::Node*>{}, nullptr, nullptr,
                                        inspect_.CreateDevice(name, zx::vmo(), 0));
  }

  std::unique_ptr<CompositeNodeSpecManager> composite_node_spec_manager_;
  InspectManager inspect_ = InspectManager(nullptr);
  FakeDeviceManagerBridge bridge_;
};

TEST_F(CompositeNodeSpecManagerTest, TestAddMatchCompositeNodeSpec) {
  fidl::Arena allocator;

  fidl::VectorView<fdf::wire::BindRule> bind_rules_1(allocator, 1);
  auto prop_vals_1 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 1);
  prop_vals_1[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_1[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_1,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_1(allocator, 1);
  props_1[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::BindRule> bind_rules_2(allocator, 2);
  auto prop_vals_2 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 2);
  prop_vals_2[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_2[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_2,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_2(allocator, 1);
  props_2[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(10),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::ParentSpec> parents(allocator, 2);
  parents[0] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_1,
      .properties = props_1,
  };
  parents[1] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_2,
      .properties = props_2,
  };

  auto spec_name = "test_name";
  fdi::MatchedCompositeNodeSpecInfo match =
      MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"});

  bridge_.AddSpecMatch(spec_name, match);
  ASSERT_TRUE(AddSpec(fdf::wire::CompositeNodeSpec::Builder(allocator)
                          .name(fidl::StringView(allocator, spec_name))
                          .parents(std::move(parents))
                          .Build())
                  .is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name)->parent_specs().size());
  ASSERT_FALSE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[0]);
  ASSERT_FALSE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[1]);

  //  Bind parent spec 2.
  auto matched_node_2 = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node_2.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name, 1, {"node-0", "node-1"}));

  ASSERT_EQ(
      1u, composite_node_spec_manager_
              ->BindParentSpec(fidl::ToWire(allocator, matched_node_2), std::weak_ptr<dfv2::Node>())
              .value()
              .size());
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[1]);

  //  Bind parent spec 1.
  auto matched_node_1 = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node_1.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));

  ASSERT_OK(composite_node_spec_manager_->BindParentSpec(fidl::ToWire(allocator, matched_node_1),
                                                         std::weak_ptr<dfv2::Node>()));
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[0]);
}

TEST_F(CompositeNodeSpecManagerTest, TestBindSameNodeTwice) {
  fidl::Arena allocator;

  fidl::VectorView<fdf::wire::BindRule> bind_rules_1(allocator, 1);
  auto prop_vals_1 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 1);
  prop_vals_1[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_1[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_1,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_1(allocator, 1);
  props_1[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::BindRule> bind_rules_2(allocator, 2);
  auto prop_vals_2 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 2);
  prop_vals_2[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_2[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_2,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_2(allocator, 1);
  props_2[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(20),
      .value = fdf::wire::NodePropertyValue::WithIntValue(100),
  };

  fidl::VectorView<fdf::wire::ParentSpec> parents(allocator, 2);
  parents[0] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_1,
      .properties = props_1,
  };
  parents[1] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_2,
      .properties = props_2,
  };

  auto spec_name = "test_name";
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));
  ASSERT_TRUE(AddSpec(fdf::wire::CompositeNodeSpec::Builder(allocator)
                          .name(fidl::StringView(allocator, spec_name))
                          .parents(std::move(parents))
                          .Build())
                  .is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name)->parent_specs().size());

  ASSERT_FALSE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[0]);
  ASSERT_FALSE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[1]);

  //  Bind parent spec 1.
  auto matched_node = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));

  std::shared_ptr<dfv2::Node> node = CreateNode("node");
  ASSERT_OK(composite_node_spec_manager_->BindParentSpec(fidl::ToWire(allocator, matched_node),
                                                         std::weak_ptr<dfv2::Node>(node)));
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[0]);

  // Bind the same node.
  ASSERT_EQ(ZX_ERR_NOT_FOUND, composite_node_spec_manager_
                                  ->BindParentSpec(fidl::ToWire(allocator, matched_node),
                                                   std::weak_ptr<dfv2::Node>(node))
                                  .status_value());
}

TEST_F(CompositeNodeSpecManagerTest, TestMultibindDisabled) {
  fidl::Arena allocator;

  // Add the first composite node spec.
  auto bind_rules_1 = std::vector{
      fdf::MakeAcceptBindRule(1, 10),
  };
  auto props_1 = std::vector{
      fdf::MakeProperty(30, 1),
  };

  auto bind_rules_2 = std::vector{
      fdf::MakeAcceptBindRule(5, 10),
  };
  auto props_2 = std::vector{
      fdf::MakeProperty(20, 10),
  };

  auto parent_specs_1 = std::vector{
      fdf::ParentSpec{{
          .bind_rules = bind_rules_1,
          .properties = props_1,
      }},
      fdf::ParentSpec{{
          .bind_rules = bind_rules_2,
          .properties = props_2,
      }},
  };

  auto spec_name_1 = "test_name";
  bridge_.AddSpecMatch(spec_name_1,
                       MakeCompositeNodeSpecInfo(spec_name_1, 0, {"node-0", "node-1"}));

  auto composite_node_spec_1 = fdf::CompositeNodeSpec{{
      .name = spec_name_1,
      .parents = parent_specs_1,
  }};
  ASSERT_TRUE(AddSpec(fidl::ToWire(allocator, composite_node_spec_1)).is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs().size());

  // Add a second composite node spec with a node that's the same as one in the first composite node
  // spec.
  auto parent_specs_2 = std::vector{
      fdf::ParentSpec{{
          .bind_rules = bind_rules_2,
          .properties = props_2,
      }},
  };
  auto spec_name_2 = "test_name2";
  bridge_.AddSpecMatch(spec_name_2, MakeCompositeNodeSpecInfo(spec_name_2, 0, {"node-0"}));

  auto composite_node_spec_2 = fdf::CompositeNodeSpec{{
      .name = spec_name_2,
      .parents = parent_specs_2,
  }};
  ASSERT_TRUE(AddSpec(fidl::ToWire(allocator, composite_node_spec_2)).is_ok());
  ASSERT_EQ(1, composite_node_spec_manager_->specs().at(spec_name_2)->parent_specs().size());

  // Bind the node that's in both specs. The node should only bind to one
  // composite node spec.
  auto matched_node = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name_1, 1, {"node-0", "node-1"}));
  matched_node.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name_2, 0, {"node-0"}));

  std::shared_ptr<dfv2::Node> node_1 = CreateNode("node_1");
  std::shared_ptr<dfv2::Node> node_2 = CreateNode("node_2");

  ASSERT_EQ(1u, composite_node_spec_manager_
                    ->BindParentSpec(fidl::ToWire(allocator, matched_node),
                                     std::weak_ptr<dfv2::Node>(node_1), false)
                    .value()
                    .size());

  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs()[1]);
  ASSERT_FALSE(composite_node_spec_manager_->specs().at(spec_name_2)->parent_specs()[0]);

  // Bind the node again. Both composite node specs should now have the bound node.
  ASSERT_OK(composite_node_spec_manager_->BindParentSpec(fidl::ToWire(allocator, matched_node),
                                                         std::weak_ptr<dfv2::Node>(node_2), false));
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs()[1]);
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_2)->parent_specs()[0]);
}

TEST_F(CompositeNodeSpecManagerTest, TestMultibindEnabled) {
  fidl::Arena allocator;

  // Add the first composite node spec.
  auto bind_rules_1 = std::vector{
      fdf::MakeAcceptBindRule(1, 10),
  };
  auto props_1 = std::vector{
      fdf::MakeProperty(30, 1),
  };

  auto bind_rules_2 = std::vector{
      fdf::MakeAcceptBindRule(5, 10),
  };
  auto props_2 = std::vector{
      fdf::MakeProperty(20, 10),
  };

  auto parent_specs_1 = std::vector{
      fdf::ParentSpec{{
          .bind_rules = bind_rules_1,
          .properties = props_1,
      }},
      fdf::ParentSpec{{
          .bind_rules = bind_rules_2,
          .properties = props_2,
      }},
  };

  auto spec_name_1 = "test_name";
  bridge_.AddSpecMatch(spec_name_1,
                       MakeCompositeNodeSpecInfo(spec_name_1, 0, {"node-0", "node-1"}));

  auto composite_node_spec_1 = fdf::CompositeNodeSpec{{
      .name = spec_name_1,
      .parents = parent_specs_1,
  }};
  ASSERT_TRUE(AddSpec(fidl::ToWire(allocator, composite_node_spec_1)).is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs().size());

  // Add a second composite node spec with a node that's the same as one in the first composite node
  // spec.
  auto parent_specs_2 = std::vector{
      fdf::ParentSpec{{
          .bind_rules = bind_rules_2,
          .properties = props_2,
      }},
  };

  auto spec_name_2 = "test_name2";
  bridge_.AddSpecMatch(spec_name_2, MakeCompositeNodeSpecInfo(spec_name_2, 0, {"node-0"}));

  auto composite_node_spec_2 = fdf::CompositeNodeSpec{{
      .name = spec_name_2,
      .parents = parent_specs_2,
  }};
  ASSERT_TRUE(AddSpec(fidl::ToWire(allocator, composite_node_spec_2)).is_ok());
  ASSERT_EQ(1, composite_node_spec_manager_->specs().at(spec_name_2)->parent_specs().size());

  // Bind the node that's in both specs. The node should bind to both.
  auto matched_node = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name_1, 1, {"node-0", "node-1"}));
  matched_node.specs()->push_back(MakeCompositeNodeSpecInfo(spec_name_2, 0, {"node-0"}));

  ASSERT_EQ(2u, composite_node_spec_manager_
                    ->BindParentSpec(fidl::ToWire(allocator, matched_node),
                                     std::weak_ptr<dfv2::Node>(), true)
                    .value()
                    .size());

  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs()[1]);
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_2)->parent_specs()[0]);
}

TEST_F(CompositeNodeSpecManagerTest, TestBindWithNoCompositeMatch) {
  fidl::Arena allocator;

  fidl::VectorView<fdf::wire::BindRule> bind_rules_1(allocator, 1);
  auto prop_vals_1 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 1);
  prop_vals_1[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_1[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_1,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_1(allocator, 1);
  props_1[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::BindRule> bind_rules_2(allocator, 2);
  auto prop_vals_2 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 2);
  prop_vals_2[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_2[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_2,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_2(allocator, 1);
  props_2[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(10),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::ParentSpec> parents(allocator, 2);
  parents[0] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_1,
      .properties = props_1,
  };
  parents[1] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_2,
      .properties = props_2,
  };

  auto spec_name = "test_name";
  auto spec = fdf::wire::CompositeNodeSpec::Builder(allocator)
                  .name(fidl::StringView(allocator, spec_name))
                  .parents(std::move(parents))
                  .Build();
  ASSERT_TRUE(AddSpec(spec).is_ok());
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name));

  //  Bind parent spec 1 with no composite driver.
  auto matched_node = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node.specs()->push_back(fdi::MatchedCompositeNodeSpecInfo{{
      .name = spec_name,
      .node_index = 0,
      .num_nodes = 2,
      .node_names = {{"node-0", "node-1"}},
  }});
  ASSERT_EQ(ZX_ERR_NOT_FOUND,
            composite_node_spec_manager_
                ->BindParentSpec(fidl::ToWire(allocator, matched_node), std::weak_ptr<dfv2::Node>())
                .status_value());

  // Add a composite match into the matched node info.
  // Reattempt binding the parent spec 1. With a matched composite driver, it should
  // now bind successfully.
  auto matched_node_with_composite = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_node_with_composite.specs()->push_back(
      MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));
  ASSERT_OK(composite_node_spec_manager_->BindParentSpec(
      fidl::ToWire(allocator, matched_node_with_composite), std::weak_ptr<dfv2::Node>()));
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name)->parent_specs().size());
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[0]);
}

TEST_F(CompositeNodeSpecManagerTest, TestAddDuplicate) {
  fidl::Arena allocator;

  fidl::VectorView<fdf::wire::BindRule> bind_rules_1(allocator, 1);
  auto prop_vals_1 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 1);
  prop_vals_1[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_1[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_1,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_1(allocator, 1);
  props_1[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::ParentSpec> parents(allocator, 1);
  parents[0] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_1,
      .properties = props_1,
  };

  fidl::VectorView<fdf::wire::ParentSpec> parents_2(allocator, 1);
  parents_2[0] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_1,
      .properties = props_1,
  };

  auto spec_name = "test_name";
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0"}));

  auto spec = fdf::wire::CompositeNodeSpec::Builder(allocator)
                  .name(fidl::StringView(allocator, spec_name))
                  .parents(std::move(parents))
                  .Build();
  ASSERT_TRUE(AddSpec(spec).is_ok());

  auto spec_2 = fdf::wire::CompositeNodeSpec::Builder(allocator)
                    .name(fidl::StringView(allocator, spec_name))
                    .parents(std::move(parents_2))
                    .Build();
  ASSERT_EQ(fuchsia_driver_framework::CompositeNodeSpecError::kAlreadyExists,
            AddSpec(spec_2).error_value());
}

TEST_F(CompositeNodeSpecManagerTest, TestRebindCompositeMatch) {
  fidl::Arena allocator;

  fidl::VectorView<fdf::wire::BindRule> bind_rules_1(allocator, 1);
  auto prop_vals_1 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 1);
  prop_vals_1[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_1[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_1,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_1(allocator, 1);
  props_1[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .value = fdf::wire::NodePropertyValue::WithIntValue(1),
  };

  fidl::VectorView<fdf::wire::BindRule> bind_rules_2(allocator, 2);
  auto prop_vals_2 = fidl::VectorView<fdf::wire::NodePropertyValue>(allocator, 2);
  prop_vals_2[0] = fdf::wire::NodePropertyValue::WithIntValue(10);
  bind_rules_2[0] = fdf::wire::BindRule{
      .key = fdf::wire::NodePropertyKey::WithIntValue(1),
      .condition = fdf::wire::Condition::kAccept,
      .values = prop_vals_2,
  };

  fidl::VectorView<fdf::wire::NodeProperty> props_2(allocator, 1);
  props_2[0] = fdf::wire::NodeProperty{
      .key = fdf::wire::NodePropertyKey::WithIntValue(100),
      .value = fdf::wire::NodePropertyValue::WithIntValue(10),
  };

  fidl::VectorView<fdf::wire::ParentSpec> parents(allocator, 2);
  parents[0] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_1,
      .properties = props_1,
  };
  parents[1] = fdf::wire::ParentSpec{
      .bind_rules = bind_rules_2,
      .properties = props_2,
  };

  auto spec_name = "test_name";
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));

  auto spec = fdf::wire::CompositeNodeSpec::Builder(allocator)
                  .name(fidl::StringView(allocator, spec_name))
                  .parents(std::move(parents))
                  .Build();
  ASSERT_TRUE(AddSpec(spec).is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name)->parent_specs().size());

  ASSERT_EQ(fuchsia_driver_framework::CompositeNodeSpecError::kAlreadyExists,
            AddSpec(spec).error_value());
}

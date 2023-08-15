// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
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

  void RemoveImpl(RemoveCompositeNodeCallback callback) override {
    remove_invoked_ = true;
    callback(zx::ok());
  }

  bool remove_invoked() const { return remove_invoked_; }

 private:
  bool remove_invoked_ = false;
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

  void RequestRebindFromDriverIndex(std::string spec, std::optional<std::string> driver_url_suffix,
                                    RequestRebindCallback callback) override {
    callback(zx::ok(fuchsia_driver_index::DriverIndexRebindCompositeNodeSpecResponse{}));
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

  fdf::ParentSpec MakeParentSpec(std::vector<fdf::BindRule> bind_rules,
                                 std::vector<fdf::NodeProperty> properties) {
    return fdf::ParentSpec{{
        .bind_rules = bind_rules,
        .properties = properties,
    }};
  }

  fit::result<fuchsia_driver_framework::CompositeNodeSpecError> AddSpec(
      fidl::AnyArena& arena, std::string name, std::vector<fdf::ParentSpec> parents) {
    auto spec = std::make_unique<FakeCompositeNodeSpec>(CompositeNodeSpecCreateInfo{
        .name = name,
        .size = parents.size(),
    });
    auto spec_ptr = spec.get();
    auto result = composite_node_spec_manager_->AddSpec(fidl::ToWire(arena, fdf::CompositeNodeSpec{{
                                                                                .name = name,
                                                                                .parents = parents,
                                                                            }}),
                                                        std::move(spec));
    if (result.is_ok()) {
      specs_[name] = spec_ptr;
    }

    return result;
  }

  std::shared_ptr<dfv2::Node> CreateNode(const char* name) {
    return std::make_shared<dfv2::Node>("node", std::vector<std::weak_ptr<dfv2::Node>>{}, nullptr,
                                        loop_.dispatcher(),
                                        inspect_.CreateDevice(name, zx::vmo(), 0));
  }

  void VerifyRemoveInvokedForSpec(bool expected, const std::string& name) {
    ZX_ASSERT(specs_[name]);
    ASSERT_EQ(expected, specs_[name]->remove_invoked());
  }

  std::unique_ptr<CompositeNodeSpecManager> composite_node_spec_manager_;

  std::unordered_map<std::string, FakeCompositeNodeSpec*> specs_;
  InspectManager inspect_ = InspectManager(nullptr);
  FakeDeviceManagerBridge bridge_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

TEST_F(CompositeNodeSpecManagerTest, TestAddMatchCompositeNodeSpec) {
  fidl::Arena allocator;

  std::vector<fdf::ParentSpec> parents{
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(1, 1)}),
      MakeParentSpec({fdf::MakeAcceptBindRule(1, 10)}, {fdf::MakeProperty(10, 1)}),
  };

  auto spec_name = "test_name";
  fdi::MatchedCompositeNodeSpecInfo match =
      MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"});

  bridge_.AddSpecMatch(spec_name, match);
  ASSERT_TRUE(AddSpec(allocator, spec_name, std::move(parents)).is_ok());
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
              .completed_node_and_drivers.size());
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

  std::vector<fdf::ParentSpec> parents{
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(1, 1)}),
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(20, 100)}),
  };

  auto spec_name = "test_name";
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));
  ASSERT_TRUE(AddSpec(allocator, spec_name, std::move(parents)).is_ok());
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

  auto shared_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(5, 10),
  };
  auto shared_props = std::vector{
      fdf::MakeProperty(20, 10),
  };

  // Add the first composite node spec.
  std::vector<fdf::ParentSpec> parent_specs_1{
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(30, 1)}),
      MakeParentSpec(shared_bind_rules, shared_props),
  };

  auto spec_name_1 = "test_name";
  bridge_.AddSpecMatch(spec_name_1,
                       MakeCompositeNodeSpecInfo(spec_name_1, 0, {"node-0", "node-1"}));
  ASSERT_TRUE(AddSpec(allocator, spec_name_1, parent_specs_1).is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs().size());

  // Add a second composite node spec with a node that's the same as one in the first composite node
  // spec.
  std::vector<fdf::ParentSpec> parent_specs_2{
      MakeParentSpec(shared_bind_rules, shared_props),
  };
  auto spec_name_2 = "test_name2";
  bridge_.AddSpecMatch(spec_name_2, MakeCompositeNodeSpecInfo(spec_name_2, 0, {"node-0"}));
  ASSERT_TRUE(AddSpec(allocator, spec_name_2, parent_specs_2).is_ok());
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
                    .completed_node_and_drivers.size());

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

  auto shared_bind_rules = std::vector{
      fdf::MakeAcceptBindRule(5, 10),
  };
  auto shared_props = std::vector{
      fdf::MakeProperty(20, 10),
  };

  // Add the first composite node spec.
  std::vector<fdf::ParentSpec> parent_specs_1{
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(30, 1)}),
      MakeParentSpec(shared_bind_rules, shared_props),
  };

  auto spec_name_1 = "test_name";
  bridge_.AddSpecMatch(spec_name_1,
                       MakeCompositeNodeSpecInfo(spec_name_1, 0, {"node-0", "node-1"}));
  ASSERT_TRUE(AddSpec(allocator, spec_name_1, parent_specs_1).is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs().size());

  // Add a second composite node spec with a node that's the same as one in the first composite node
  // spec.
  std::vector<fdf::ParentSpec> parent_specs_2{
      MakeParentSpec(shared_bind_rules, shared_props),
  };
  auto spec_name_2 = "test_name2";
  bridge_.AddSpecMatch(spec_name_2, MakeCompositeNodeSpecInfo(spec_name_2, 0, {"node-0"}));
  ASSERT_TRUE(AddSpec(allocator, spec_name_2, parent_specs_2).is_ok());
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
                    .completed_node_and_drivers.size());

  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_1)->parent_specs()[1]);
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name_2)->parent_specs()[0]);
}

TEST_F(CompositeNodeSpecManagerTest, TestBindWithNoCompositeMatch) {
  fidl::Arena allocator;
  std::vector<fdf::ParentSpec> parent_specs{
      MakeParentSpec({fdf::MakeAcceptBindRule(1, 10)}, {fdf::MakeProperty(1, 1)}),
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(10, 1)}),
  };

  auto spec_name = "test_name";
  ASSERT_TRUE(AddSpec(allocator, spec_name, parent_specs).is_ok());
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
  std::vector<fdf::ParentSpec> parent_specs{
      MakeParentSpec({fdf::MakeAcceptBindRule(1, 10)}, {fdf::MakeProperty(1, 1)}),
  };

  auto spec_name = "test_name";
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0"}));
  ASSERT_TRUE(AddSpec(allocator, spec_name, parent_specs).is_ok());
  ASSERT_EQ(fuchsia_driver_framework::CompositeNodeSpecError::kAlreadyExists,
            AddSpec(allocator, spec_name, std::move(parent_specs)).error_value());
}

TEST_F(CompositeNodeSpecManagerTest, TestDuplicateSpecsWithMatch) {
  fidl::Arena allocator;
  std::vector<fdf::ParentSpec> parent_specs{
      MakeParentSpec({fdf::MakeAcceptBindRule(1, 10)}, {fdf::MakeProperty(1, 1)}),
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(100, 10)}),
  };

  auto spec_name = "test_name";
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));

  ASSERT_TRUE(AddSpec(allocator, spec_name, parent_specs).is_ok());
  ASSERT_EQ(2, composite_node_spec_manager_->specs().at(spec_name)->parent_specs().size());
  ASSERT_EQ(fuchsia_driver_framework::CompositeNodeSpecError::kAlreadyExists,
            AddSpec(allocator, spec_name, std::move(parent_specs)).error_value());
}

TEST_F(CompositeNodeSpecManagerTest, TestRebindRequestWithNoMatch) {
  fidl::Arena allocator;
  std::vector<fdf::ParentSpec> parent_specs{
      MakeParentSpec({fdf::MakeAcceptBindRule(1, 10)}, {fdf::MakeProperty(1, 1)}),
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(100, 10)}),
  };

  std::string spec_name = "test_name";
  ASSERT_TRUE(AddSpec(allocator, spec_name, parent_specs).is_ok());

  bool is_callback_success = false;
  composite_node_spec_manager_->Rebind(spec_name, std::nullopt,
                                       [&is_callback_success](zx::result<> result) {
                                         if (result.is_ok()) {
                                           is_callback_success = true;
                                         }
                                       });
  ASSERT_TRUE(is_callback_success);
  VerifyRemoveInvokedForSpec(true, spec_name);
}

TEST_F(CompositeNodeSpecManagerTest, TestRebindRequestWithMatch) {
  fidl::Arena allocator;
  std::vector<fdf::ParentSpec> parent_specs{
      MakeParentSpec({fdf::MakeAcceptBindRule(1, 10)}, {fdf::MakeProperty(1, 1)}),
      MakeParentSpec({fdf::MakeAcceptBindRule(10, 1)}, {fdf::MakeProperty(100, 10)}),
  };

  std::string spec_name = "test_name";
  ASSERT_TRUE(AddSpec(allocator, spec_name, parent_specs).is_ok());
  bridge_.AddSpecMatch(spec_name, MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));

  auto matched_parent_1 = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_parent_1.specs()->push_back(
      MakeCompositeNodeSpecInfo(spec_name, 0, {"node-0", "node-1"}));
  ASSERT_EQ(1u, composite_node_spec_manager_
                    ->BindParentSpec(fidl::ToWire(allocator, matched_parent_1),
                                     std::weak_ptr<dfv2::Node>())
                    .value()
                    .completed_node_and_drivers.size());
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[0]);

  auto matched_parent_2 = fdi::MatchedCompositeNodeParentInfo{{
      .specs = std::vector<fdi::MatchedCompositeNodeSpecInfo>(),
  }};
  matched_parent_2.specs()->push_back(
      MakeCompositeNodeSpecInfo(spec_name, 1, {"node-0", "node-1"}));
  ASSERT_EQ(1u, composite_node_spec_manager_
                    ->BindParentSpec(fidl::ToWire(allocator, matched_parent_2),
                                     std::weak_ptr<dfv2::Node>())
                    .value()
                    .completed_node_and_drivers.size());
  ASSERT_TRUE(composite_node_spec_manager_->specs().at(spec_name)->parent_specs()[1]);

  bool is_callback_success = false;
  composite_node_spec_manager_->Rebind(spec_name, std::nullopt,
                                       [&is_callback_success](zx::result<> result) {
                                         if (result.is_ok()) {
                                           is_callback_success = true;
                                         }
                                       });
  ASSERT_TRUE(is_callback_success);
  VerifyRemoveInvokedForSpec(true, spec_name);
}

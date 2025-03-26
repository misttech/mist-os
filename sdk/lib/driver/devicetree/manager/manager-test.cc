// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "manager.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/default/default.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/registry.h>
#include <zircon/errors.h>

#include <cstddef>
#include <memory>
#include <optional>
#include <unordered_set>
#include <utility>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/devicetree/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <gtest/gtest.h>

#include "manager-test-helper.h"
#include "test-data/basic-properties.h"
#include "test-data/simple.h"
#include "visitor.h"

namespace fdf_devicetree {
namespace {

class ManagerTest : public testing::ManagerTestHelper, public ::testing::Test {
 public:
  ManagerTest() : ManagerTestHelper("ManagerTest") {}
};

TEST_F(ManagerTest, TestFindsNodes) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/simple.dtb"));
  class EmptyVisitor : public Visitor {
   public:
    zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      return zx::ok();
    }
  };
  EmptyVisitor visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  ASSERT_EQ(3lu, manager.nodes().size());

  // Root node is always first, and has no name.
  Node* node = manager.nodes()[0].get();
  ASSERT_STREQ("dt-root", node->name().data());

  // example-device node should be next.
  node = manager.nodes()[1].get();
  ASSERT_STREQ("example-device", node->name().data());

  // another-device should be last.
  node = manager.nodes()[2].get();
  ASSERT_STREQ("another-device", node->name().data());
}

TEST_F(ManagerTest, TestPropertyCallback) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/simple.dtb"));
  class TestVisitor : public Visitor {
   public:
    zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      for (auto& [name, _] : node.properties()) {
        if (node.name() == "example-device") {
          auto iter = expected.find(std::string(name));
          EXPECT_NE(expected.end(), iter) << "Property " << name << " was unexpected.";
          if (iter != expected.end()) {
            expected.erase(iter);
          }
        }
      }
      return zx::ok();
    }

    std::unordered_set<std::string> expected{
        "compatible",
        "phandle",
    };
  };

  TestVisitor visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  EXPECT_EQ(0lu, visitor.expected.size());
}

TEST_F(ManagerTest, TestPublishesSimpleNode) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/simple.dtb"));
  DefaultVisitors<> default_visitors;
  ASSERT_EQ(ZX_OK, manager.Walk(default_visitors).status_value());

  ASSERT_TRUE(DoPublish(manager).is_ok());
  ASSERT_EQ(0lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));
  ASSERT_EQ(2lu, env().SyncCall(&testing::FakeEnvWrapper::non_pbus_node_size));

  ASSERT_EQ(0lu, env().SyncCall(&testing::FakeEnvWrapper::mgr_requests_size));

  auto non_pbus_node_0 = env().SyncCall(&testing::FakeEnvWrapper::non_pbus_nodes_at, 0);
  ASSERT_TRUE(non_pbus_node_0->args().name().has_value());
  ASSERT_EQ(non_pbus_node_0->args().name(), "dt-root");
  ASSERT_TRUE(non_pbus_node_0->args().properties().has_value());

  ASSERT_TRUE(testing::CheckHasProperties(
      {{{
          .key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
              bind_fuchsia_devicetree::FIRST_COMPATIBLE),
          .value =
              fuchsia_driver_framework::NodePropertyValue::WithStringValue("fuchsia,sample-dt"),
      }}},
      *non_pbus_node_0->args().properties(), false));

  auto non_pbus_node_1 = env().SyncCall(&testing::FakeEnvWrapper::non_pbus_nodes_at, 1);
  ASSERT_TRUE(non_pbus_node_1->args().name().has_value());
  ASSERT_NE(nullptr, strstr("example-device", non_pbus_node_1->args().name()->data()));
  ASSERT_TRUE(non_pbus_node_1->args().properties().has_value());

  ASSERT_TRUE(testing::CheckHasProperties(
      {{{
          .key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
              bind_fuchsia_devicetree::FIRST_COMPATIBLE),
          .value =
              fuchsia_driver_framework::NodePropertyValue::WithStringValue("fuchsia,sample-device"),
      }}},
      *non_pbus_node_1->args().properties(), false));
}

TEST_F(ManagerTest, DriverVisitorTest) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/basic-properties.dtb"));

  class TestDriverVisitor final : public DriverVisitor {
   public:
    TestDriverVisitor() : DriverVisitor({"wrong-string", "fuchsia,sample-device"}) {}

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      visited = true;
      return zx::ok();
    }
    bool visited = false;
  };

  TestDriverVisitor visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());

  ASSERT_TRUE(DoPublish(manager).is_ok());
  ASSERT_TRUE(visitor.visited);
}

TEST_F(ManagerTest, TestMetadata) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/basic-properties.dtb"));

  class MetadataVisitor : public DriverVisitor {
   public:
    MetadataVisitor() : DriverVisitor({"fuchsia,sample-device"}) {}

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      auto prop = node.properties().find("device_specific_prop");
      EXPECT_NE(node.properties().end(), prop) << "Property device_specific_prop was unexpected.";
      device_specific_prop = prop->second.AsUint32().value_or(ZX_ERR_INVALID_ARGS);
      EXPECT_EQ(device_specific_prop, (uint32_t)DEVICE_SPECIFIC_PROP_VALUE);
      fuchsia_hardware_platform_bus::Metadata metadata = {
          {.data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&device_specific_prop),
                                        reinterpret_cast<const uint8_t*>(&device_specific_prop) +
                                            sizeof(device_specific_prop))}};
      node.AddMetadata(metadata);

      return zx::ok();
    }
    uint32_t device_specific_prop = 0;
  };

  DefaultVisitors<MetadataVisitor> visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());

  ASSERT_TRUE(DoPublish(manager).is_ok());

  ASSERT_EQ(1lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));
  ASSERT_EQ(9lu, env().SyncCall(&testing::FakeEnvWrapper::non_pbus_node_size));

  // Check metadata of sample-device.
  auto metadata = env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, 0).metadata();

  // Test Metadata properties.
  ASSERT_TRUE(metadata);
  ASSERT_EQ(1lu, metadata->size());
  ASSERT_EQ((uint32_t)DEVICE_SPECIFIC_PROP_VALUE,
            *reinterpret_cast<uint32_t*>((*(*metadata)[0].data()).data()));
}

TEST_F(ManagerTest, TestReferences) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/basic-properties.dtb"));

  class ReferenceParentVisitor final : public DriverVisitor {
   public:
    using Property1Specifier = devicetree::PropEncodedArrayElement<PROPERTY1_CELLS>;

    ReferenceParentVisitor() : DriverVisitor({"fuchsia,reference-parent"}) {
      Properties props = {};
      props.emplace_back(std::make_unique<ReferenceProperty>("property1", "#property1-cells"));
      props.emplace_back(std::make_unique<ReferenceProperty>("property2", "#property2-cells"));
      props.emplace_back(std::make_unique<StringListProperty>("property2-names"));
      parser_ = std::make_unique<PropertyParser>(std::move(props));
    }

    zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      zx::result<PropertyValues> parser_output = parser_->Parse(node);
      if (parser_output.is_error()) {
        return parser_output.take_error();
      }

      if (auto iter = parser_output->find("property1"); iter != parser_output->end()) {
        for (auto& value : iter->second) {
          auto reference = value.AsReference();
          if (reference && is_match(reference->first.properties())) {
            reference1_specifier() =
                devicetree::PropEncodedArray<Property1Specifier>(reference->second, 1);
            reference1_count()++;
          }
        }
      }

      if (auto iter = parser_output->find("property2");
          iter != parser_output->end() && parser_output->contains("property2-names")) {
        size_t index = 0;
        for (auto& value : iter->second) {
          auto reference = value.AsReference();
          if (reference && is_match(reference->first.properties())) {
            auto name = (*parser_output)["property2-names"][index].AsString().value();
            reference2_names().emplace_back(name);
            reference2_parent_names().push_back(reference->first.name());
            reference2_count()++;
          }
          index++;
        }
      }

      return DriverVisitor::Visit(node, decoder);
    }

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      visit_called()++;
      return zx::ok();
    }

    zx::result<> DriverFinalizeNode(Node& node) override {
      ZX_ASSERT(reference1_count() == 1u);
      ZX_ASSERT(reference2_count() == 3u);
      finalize_called()++;
      return zx::ok();
    }

    size_t& visit_called() { return visit_called_; }
    size_t& finalize_called() { return finalize_called_; }
    size_t& reference1_count() { return reference1_count_; }
    size_t& reference2_count() { return reference2_count_; }
    devicetree::PropEncodedArray<Property1Specifier>& reference1_specifier() {
      return reference1_specifier_;
    }
    std::vector<std::string>& reference2_names() { return reference2_names_; }
    std::vector<std::string>& reference2_parent_names() { return reference2_parent_names_; }

   private:
    size_t visit_called_ = 0;
    size_t finalize_called_ = 0;
    size_t reference1_count_ = 0;
    size_t reference2_count_ = 0;
    devicetree::PropEncodedArray<Property1Specifier> reference1_specifier_;
    std::vector<std::string> reference2_names_;
    std::vector<std::string> reference2_parent_names_;
    std::unique_ptr<PropertyParser> parser_;
  };

  auto parent_visitor = std::make_unique<ReferenceParentVisitor>();
  ReferenceParentVisitor* parent_visitor_ptr = parent_visitor.get();

  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(parent_visitor)).is_ok());

  ASSERT_EQ(ZX_OK, manager.Walk(visitors).status_value());

  ASSERT_EQ(parent_visitor_ptr->visit_called(), 3u);
  ASSERT_EQ(parent_visitor_ptr->finalize_called(), 3u);

  ASSERT_EQ(parent_visitor_ptr->reference1_specifier().size(), 1u);
  ASSERT_EQ(parent_visitor_ptr->reference1_specifier()[0][0], PROPERTY1_SPECIFIER);

  ASSERT_EQ(parent_visitor_ptr->reference2_parent_names()[0], "reference-parent-1");
  ASSERT_EQ(parent_visitor_ptr->reference2_parent_names()[1], "reference-parent-2");
  ASSERT_EQ(parent_visitor_ptr->reference2_parent_names()[2], "reference-parent-3");
  ASSERT_EQ(parent_visitor_ptr->reference2_names()[0], PROPERTY2_NAME1);
  ASSERT_EQ(parent_visitor_ptr->reference2_names()[1], PROPERTY2_NAME2);
  ASSERT_EQ(parent_visitor_ptr->reference2_names()[2], PROPERTY2_NAME3);

  ASSERT_TRUE(DoPublish(manager).is_ok());
}

TEST_F(ManagerTest, TestParentChild) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/basic-properties.dtb"));

  class ParentVisitor final : public DriverVisitor {
   public:
    ParentVisitor() : DriverVisitor({"fuchsia,parent"}) {}

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      auto children = node.children();
      child_count = children.size();
      for (ChildNode& child : children) {
        child_names.push_back(child.name());
      }
      name = node.name();
      return zx::ok();
    }
    size_t child_count = 0;
    std::vector<std::string_view> child_names;
    std::string_view name;
  };

  class ChildVisitor final : public DriverVisitor {
   public:
    ChildVisitor() : DriverVisitor({"fuchsia,child"}) {}

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      count++;
      if (!parent_name.empty() && parent_name != node.parent().name()) {
        return zx::error(ZX_ERR_INTERNAL);
      }
      parent_name = node.parent().name();
      names.push_back(node.name());
      return zx::ok();
    }
    size_t count = 0;
    std::vector<std::string_view> names;
    std::string_view parent_name;
  };

  auto parent_visitor = std::make_unique<ParentVisitor>();
  ParentVisitor* parent_visitor_ptr = parent_visitor.get();

  auto child_visitor = std::make_unique<ChildVisitor>();
  ChildVisitor* child_visitor_ptr = child_visitor.get();

  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(parent_visitor)).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(child_visitor)).is_ok());

  ASSERT_EQ(ZX_OK, manager.Walk(visitors).status_value());

  EXPECT_EQ(parent_visitor_ptr->child_count, child_visitor_ptr->count);
  EXPECT_EQ(child_visitor_ptr->count, 2u);
  for (auto child_name : parent_visitor_ptr->child_names) {
    bool matched = false;
    for (auto name : child_visitor_ptr->names) {
      if (name == child_name) {
        matched = true;
        break;
      }
    }
    EXPECT_TRUE(matched);
  }
  EXPECT_EQ(child_visitor_ptr->parent_name, parent_visitor_ptr->name);

  ASSERT_TRUE(DoPublish(manager).is_ok());
}

TEST_F(ManagerTest, TestSkipDisabledNodes) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/status-disabled.dtb"));
  DefaultVisitors<> default_visitors;
  ASSERT_EQ(ZX_OK, manager.Walk(default_visitors).status_value());

  ASSERT_TRUE(DoPublish(manager).is_ok());
  ASSERT_EQ(0lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));
  ASSERT_EQ(3lu, env().SyncCall(&testing::FakeEnvWrapper::non_pbus_node_size));

  auto non_pbus_node0 = env().SyncCall(&testing::FakeEnvWrapper::non_pbus_nodes_at, 0);
  ASSERT_TRUE(non_pbus_node0->args().name().has_value());
  ASSERT_EQ(non_pbus_node0->args().name(), "dt-root");
  auto non_pbus_node1 = env().SyncCall(&testing::FakeEnvWrapper::non_pbus_nodes_at, 1);
  ASSERT_TRUE(non_pbus_node1->args().name().has_value());
  ASSERT_NE(nullptr, strstr("status-okay-device", non_pbus_node1->args().name()->data()));
  auto non_pbus_node2 = env().SyncCall(&testing::FakeEnvWrapper::non_pbus_nodes_at, 2);
  ASSERT_TRUE(non_pbus_node2->args().name().has_value());
  ASSERT_NE(nullptr, strstr("status-none-device", non_pbus_node2->args().name()->data()));
}

TEST_F(ManagerTest, TestNonPbusCompositeSpec) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/simple.dtb"));
  static const std::string kTestKey = "test-key";
  static const std::string kTestProperty = "test-property";

  class TestDriverVisitor final : public DriverVisitor {
   public:
    TestDriverVisitor() : DriverVisitor({SAMPLE_DEVICE_COMPATIBILITY}) {}

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      visited = true;
      parent_spec.bind_rules({fdf::MakeAcceptBindRule(kTestKey, kTestProperty)});
      parent_spec.properties({fdf::MakeProperty(kTestKey, kTestProperty)});
      node.AddNodeSpec(parent_spec);
      return zx::ok();
    }
    bool visited = false;
    fuchsia_driver_framework::ParentSpec parent_spec;
  };

  DefaultVisitors<TestDriverVisitor> visitor;

  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  ASSERT_TRUE(DoPublish(manager).is_ok());

  ASSERT_EQ(0lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));
  ASSERT_EQ(2lu, env().SyncCall(&testing::FakeEnvWrapper::non_pbus_node_size));
  ASSERT_EQ(1lu, env().SyncCall(&testing::FakeEnvWrapper::mgr_requests_size));

  auto mgr_request = env().SyncCall(&testing::FakeEnvWrapper::mgr_requests_at, 0);
  ASSERT_TRUE(mgr_request.parents().has_value());
  ASSERT_EQ(2lu, mgr_request.parents()->size());

  EXPECT_TRUE(testing::CheckHasProperties(
      {{
          fdf::MakeProperty(bind_fuchsia_devicetree::FIRST_COMPATIBLE, SAMPLE_DEVICE_COMPATIBILITY),
      }},
      (*mgr_request.parents())[0].properties(), true));
  EXPECT_TRUE(testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_devicetree::FIRST_COMPATIBLE,
                                  SAMPLE_DEVICE_COMPATIBILITY),
      },
      (*mgr_request.parents())[0].bind_rules(), true));

  EXPECT_TRUE(testing::CheckHasProperties({{fdf::MakeProperty(kTestKey, kTestProperty)}},
                                          (*mgr_request.parents())[1].properties(), false));
  EXPECT_TRUE(testing::CheckHasBindRules({{fdf::MakeAcceptBindRule(kTestKey, kTestProperty)}},
                                         (*mgr_request.parents())[1].bind_rules(), false));
}

TEST_F(ManagerTest, TestPbusCompositeSpec) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/simple.dtb"));
  static const std::string kTestKey = "test-key";
  static const std::string kTestProperty = "test-property";

  class TestDriverVisitor final : public DriverVisitor {
   public:
    TestDriverVisitor() : DriverVisitor({SAMPLE_DEVICE_COMPATIBILITY}) {}

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      visited = true;
      parent_spec.bind_rules({fdf::MakeAcceptBindRule(kTestKey, kTestProperty)});
      parent_spec.properties({fdf::MakeProperty(kTestKey, kTestProperty)});
      node.AddNodeSpec(parent_spec);
      // This adds a pbus resource, making the one of the parent of the composite to be platform
      // device.
      node.AddBootMetadata({});
      return zx::ok();
    }
    bool visited = false;
    fuchsia_driver_framework::ParentSpec parent_spec;
  };

  DefaultVisitors<TestDriverVisitor> visitor;

  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  ASSERT_TRUE(DoPublish(manager).is_ok());

  ASSERT_EQ(1lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));
  ASSERT_EQ(1lu, env().SyncCall(&testing::FakeEnvWrapper::non_pbus_node_size));
  ASSERT_EQ(1lu, env().SyncCall(&testing::FakeEnvWrapper::mgr_requests_size));

  auto mgr_request = env().SyncCall(&testing::FakeEnvWrapper::mgr_requests_at, 0);
  ASSERT_TRUE(mgr_request.parents().has_value());
  ASSERT_EQ(2lu, mgr_request.parents()->size());

  EXPECT_TRUE(testing::CheckHasProperties(
      {{
          fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      }},
      (*mgr_request.parents())[0].properties(), true));
  EXPECT_TRUE(testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                  bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      },
      (*mgr_request.parents())[0].bind_rules(), true));

  EXPECT_TRUE(testing::CheckHasProperties({{fdf::MakeProperty(kTestKey, kTestProperty)}},
                                          (*mgr_request.parents())[1].properties(), false));
  EXPECT_TRUE(testing::CheckHasBindRules({{fdf::MakeAcceptBindRule(kTestKey, kTestProperty)}},
                                         (*mgr_request.parents())[1].bind_rules(), false));
}

TEST_F(ManagerTest, TestPublishOrder) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/simple.dtb"));
  DefaultVisitors<> visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  auto& first_node = manager.nodes()[0];
  auto first_node_id = first_node->id();
  auto& second_node = manager.nodes()[1];
  auto second_node_id = second_node->id();
  EXPECT_EQ(first_node->GetPublishIndex(), 0u);
  EXPECT_EQ(second_node->GetPublishIndex(), 1u);
  EXPECT_TRUE(first_node->ChangePublishOrder(1u).is_ok());
  EXPECT_EQ(manager.nodes()[0]->id(), second_node_id);
  EXPECT_EQ(manager.nodes()[1]->id(), first_node_id);
  ASSERT_TRUE(DoPublish(manager).is_ok());
}

}  // namespace
}  // namespace fdf_devicetree

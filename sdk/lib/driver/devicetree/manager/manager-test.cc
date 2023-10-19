// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "manager.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/devicetree/visitors/default/default.h>
#include <lib/driver/devicetree/visitors/driver-visitor.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <memory>
#include <unordered_set>

#include <bind/fuchsia/devicetree/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <gtest/gtest.h>

#include "manager-test-helper.h"
#include "test-data/basic-properties.h"
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
  ASSERT_STREQ("", node->name().data());

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
  ASSERT_EQ(2lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));

  ASSERT_EQ(0lu, env().SyncCall(&testing::FakeEnvWrapper::mgr_requests_size));

  auto pbus_node = env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, 1);
  ASSERT_TRUE(pbus_node.name().has_value());
  ASSERT_NE(nullptr, strstr("example-device", pbus_node.name()->data()));
  ASSERT_TRUE(pbus_node.properties().has_value());

  ASSERT_TRUE(testing::CheckHasProperties(
      {{{
          .key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
              bind_fuchsia_devicetree::FIRST_COMPATIBLE),
          .value =
              fuchsia_driver_framework::NodePropertyValue::WithStringValue("fuchsia,sample-device"),
      }}},
      *pbus_node.properties()));
}

TEST_F(ManagerTest, DriverVisitorTest) {
  Manager manager(testing::LoadTestBlob("/pkg/test-data/basic-properties.dtb"));

  class TestDriverVisitor final : public DriverVisitor {
   public:
    TestDriverVisitor() : DriverVisitor("fuchsia,sample-device") {}

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
    MetadataVisitor() : DriverVisitor("fuchsia,sample-device") {}

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

  ASSERT_EQ(5lu, env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));

  // First node is devicetree root. Second one is the sample-device. Check
  // metadata of sample-device.
  auto metadata = env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, 1).metadata();

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

    ReferenceParentVisitor() : DriverVisitor("fuchsia,reference-parent") {
      parser1_ = std::make_unique<ReferencePropertyParser>(
          "property1", "#property1-cells",
          [this](ReferenceNode& node) { return this->is_match(node.properties()); },
          [this](Node& child, ReferenceNode& parent, devicetree::ByteView reference_cells) {
            return this->Property1ReferenceChildVisit(child, parent, reference_cells);
          });
      parser2_ = std::make_unique<ReferencePropertyParser>(
          "property2", "#property2-cells",
          [this](ReferenceNode& node) { return this->is_match(node.properties()); },
          [this](Node& child, ReferenceNode& parent, devicetree::ByteView reference_cells) {
            return this->Property2ReferenceChildVisit(child, parent, reference_cells);
          });
      DriverVisitor::AddReferencePropertyParser(parser1_.get());
      DriverVisitor::AddReferencePropertyParser(parser2_.get());
    }

    zx::result<> DriverVisit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      visit_called++;
      return zx::ok();
    }

    zx::result<> DriverFinalizeNode(Node& node) override {
      ZX_ASSERT(reference1_count == 1u);
      ZX_ASSERT(reference2_count == 1u);
      finalize_called++;
      return zx::ok();
    }

    zx::result<> Property1ReferenceChildVisit(Node& child, ReferenceNode& parent,
                                              PropertyCells reference_cells) {
      reference1_specifier = devicetree::PropEncodedArray<Property1Specifier>(reference_cells, 1);
      reference1_count++;
      return zx::ok();
    }

    zx::result<> Property2ReferenceChildVisit(Node& child, ReferenceNode& parent,
                                              PropertyCells reference_cells) {
      reference2_count++;
      return zx::ok();
    }

    size_t visit_called = 0;
    size_t finalize_called = 0;
    size_t reference1_count = 0;
    size_t reference2_count = 0;
    devicetree::PropEncodedArray<Property1Specifier> reference1_specifier;

   private:
    std::unique_ptr<ReferencePropertyParser> parser1_;
    std::unique_ptr<ReferencePropertyParser> parser2_;
  };

  auto parent_visitor = std::make_unique<ReferenceParentVisitor>();
  ReferenceParentVisitor* parent_visitor_ptr = parent_visitor.get();

  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(parent_visitor)).is_ok());

  ASSERT_EQ(ZX_OK, manager.Walk(visitors).status_value());

  ASSERT_EQ(parent_visitor_ptr->visit_called, 1u);
  ASSERT_EQ(parent_visitor_ptr->finalize_called, 1u);
  ASSERT_EQ(parent_visitor_ptr->reference1_specifier.size(), 1u);
  ASSERT_EQ(parent_visitor_ptr->reference1_specifier[0][0], PROPERTY1_SPECIFIER);

  ASSERT_TRUE(DoPublish(manager).is_ok());
}

}  // namespace
}  // namespace fdf_devicetree

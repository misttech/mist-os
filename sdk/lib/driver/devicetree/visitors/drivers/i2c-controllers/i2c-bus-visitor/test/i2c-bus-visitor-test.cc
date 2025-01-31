// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../i2c-bus-visitor.h"

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/i2c/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/i2c.h"

namespace i2c_bus_dt {

class I2cBusVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<I2cBusVisitor> {
 public:
  explicit I2cBusVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<I2cBusVisitor>(dtb_path, "I2cBusVisitorTest") {}
};

TEST(I2cBusVisitorTest, TestI2CChannels) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<I2cBusVisitorTester>("/pkg/test-data/i2c.dtb");
  I2cBusVisitorTester* i2c_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, i2c_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(i2c_tester->DoPublish().is_ok());

  auto pbus_node_count =
      i2c_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
  auto non_pbus_node_count =
      i2c_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::non_pbus_node_size);

  ASSERT_EQ(
      3lu, i2c_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < pbus_node_count; i++) {
    auto node =
        i2c_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("i2c-") != std::string::npos) {
      auto metadata = i2c_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(2lu, metadata->size());

      // I2C Channels metadata
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result decoded =
          fidl::Unpersist<fuchsia_hardware_i2c_businfo::I2CBusMetadata>(cpp20::span(metadata_blob));
      ASSERT_TRUE(decoded.is_ok());
      ASSERT_EQ(decoded->bus_id(), 0u);
      auto& channels = *decoded->channels();
      ASSERT_EQ(channels.size(), 4lu);
      EXPECT_EQ(channels[0].address(), static_cast<uint32_t>(I2C_ADDRESS1));
      EXPECT_EQ(channels[1].address(), static_cast<uint32_t>(I2C_ADDRESS2));
      EXPECT_EQ(channels[2].address(), static_cast<uint32_t>(I2C_ADDRESS3));
      EXPECT_EQ(channels[3].address(), static_cast<uint32_t>(I2C_ADDRESS4));

      // I2C Channels metadata
      metadata_blob = std::move(*(*metadata)[1].data());
      decoded =
          fidl::Unpersist<fuchsia_hardware_i2c_businfo::I2CBusMetadata>(cpp20::span(metadata_blob));
      ASSERT_TRUE(decoded.is_ok());
      ASSERT_EQ(decoded->bus_id(), 0u);
      channels = *decoded->channels();
      ASSERT_EQ(channels.size(), 4lu);
      EXPECT_EQ(channels[0].address(), static_cast<uint32_t>(I2C_ADDRESS1));
      EXPECT_EQ(channels[1].address(), static_cast<uint32_t>(I2C_ADDRESS2));
      EXPECT_EQ(channels[2].address(), static_cast<uint32_t>(I2C_ADDRESS3));
      EXPECT_EQ(channels[3].address(), static_cast<uint32_t>(I2C_ADDRESS4));

      node_tested_count++;
    }
  }

  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < non_pbus_node_count; i++) {
    std::shared_ptr<fuchsia_driver_framework::NodeAddChildRequest> node =
        i2c_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::non_pbus_nodes_at, i);

    ASSERT_TRUE(node->args().name().has_value());
    std::string node_name = node->args().name().value();
    if (node_name.find("child-") == std::string::npos) {
      continue;
    }

    fuchsia_driver_framework::CompositeNodeSpec composite_node_spec = i2c_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
    ASSERT_TRUE(composite_node_spec.parents().has_value());
    const std::vector<fuchsia_driver_framework::ParentSpec>& parent_specs =
        *composite_node_spec.parents();

    // The first parent is the pdev node and the rest parents are I2c nodes.
    ASSERT_GT(parent_specs.size(), 1lu);
    cpp20::span<const fuchsia_driver_framework::ParentSpec> i2c_nodes(++parent_specs.begin(),
                                                                      parent_specs.end());

    if (node_name == "child-c") {
      ASSERT_EQ(i2c_nodes.size(), 1lu);
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {
              fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                                bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS1}),
          },
          i2c_nodes[0].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, 0u),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS1}),
          }},
          i2c_nodes[0].bind_rules(), false));
    }

    if (node_name == "child-1e") {
      ASSERT_EQ(i2c_nodes.size(), 1lu);
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {
              {fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                                 bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT)},
              fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS2}),
          },
          i2c_nodes[0].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, 0u),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS2}),
          }},
          i2c_nodes[0].bind_rules(), false));
    }

    if (node_name == "child-2b") {
      ASSERT_EQ(i2c_nodes.size(), 2lu);

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {
              {fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                                 bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT)},
              fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS3}),
          },
          i2c_nodes[0].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, 0u),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS3}),
          }},
          i2c_nodes[0].bind_rules(), false));

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {
              {fdf::MakeProperty(bind_fuchsia_hardware_i2c::SERVICE,
                                 bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT)},
              fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS4}),
          },
          i2c_nodes[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_i2c::SERVICE,
                                      bind_fuchsia_hardware_i2c::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, 0u),
              fdf::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, uint32_t{I2C_ADDRESS4}),
          }},
          i2c_nodes[1].bind_rules(), false));
    }

    node_tested_count++;
  }

  ASSERT_EQ(node_tested_count, 4u);
}

}  // namespace i2c_bus_dt

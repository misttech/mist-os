// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../spmi-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <memory>
#include <optional>
#include <string_view>

#include <bind/fuchsia/hardware/spmi/cpp/bind.h>
#include <bind/fuchsia/spmi/cpp/bind.h>
#include <gtest/gtest.h>

namespace {

std::optional<fuchsia_hardware_spmi::TargetInfo> FindTargetById(
    uint8_t id, const fuchsia_hardware_spmi::ControllerInfo& controller) {
  if (!controller.targets()) {
    return {};
  }

  for (const auto& target : *controller.targets()) {
    if (target.id() && *target.id() == id) {
      return target;
    }
  }

  return {};
}

std::optional<fuchsia_hardware_spmi::SubTargetInfo> FindSubTargetByAddress(
    uint16_t address, const fuchsia_hardware_spmi::TargetInfo& target) {
  if (!target.sub_targets()) {
    return {};
  }

  for (const auto& sub_target : *target.sub_targets()) {
    if (sub_target.address() && *sub_target.address() == address) {
      return sub_target;
    }
  }

  return {};
}

}  // namespace

namespace spmi_dt {

class SpmiVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<SpmiVisitor> {
 public:
  explicit SpmiVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<SpmiVisitor>(dtb_path, "SpmiBusVisitorTest") {}

  std::optional<fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec>>
  FindMgrRequest(std::string_view name) {
    const size_t mgr_request_size =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size);
    for (size_t i = 0; i < mgr_request_size; i++) {
      fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec> request =
          env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, i);
      if (request.name() && *request.name() == name) {
        return request;
      }
    }

    return {};
  }

  std::optional<fuchsia_hardware_platform_bus::Node> FindPbusNode(std::string_view name) {
    const size_t pbus_node_size =
        env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
    for (size_t i = 0; i < pbus_node_size; i++) {
      fuchsia_hardware_platform_bus::Node node =
          env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
      if (node.name() && *node.name() == name) {
        return node;
      }
    }

    return {};
  }
};

TEST(SpmiVisitorTest, TwoControllers) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  SpmiVisitorTester* const spmi_tester = new SpmiVisitorTester("/pkg/test-data/spmi.dtb");
  ASSERT_TRUE(visitors.RegisterVisitor(std::unique_ptr<SpmiVisitorTester>{spmi_tester}).is_ok());

  ASSERT_TRUE(spmi_tester->manager()->Walk(visitors).is_ok());
  ASSERT_TRUE(spmi_tester->DoPublish().is_ok());

  // First controller metadata
  const auto pbus_node_0 = spmi_tester->FindPbusNode("spmi-abcd0000");
  ASSERT_TRUE(pbus_node_0);

  ASSERT_TRUE(pbus_node_0->metadata());
  ASSERT_EQ(pbus_node_0->metadata()->size(), 1u);

  ASSERT_TRUE((*pbus_node_0->metadata())[0].type());
  EXPECT_EQ(*(*pbus_node_0->metadata())[0].type(),
            fuchsia_hardware_spmi::kControllerInfoMetadataType);

  ASSERT_TRUE((*pbus_node_0->metadata())[0].data());
  const std::vector<uint8_t>& metadata_0 = *(*pbus_node_0->metadata())[0].data();

  const auto controller_0 = fidl::Unpersist<fuchsia_hardware_spmi::ControllerInfo>(
      {metadata_0.data(), metadata_0.size()});
  ASSERT_TRUE(controller_0.is_ok());

  ASSERT_TRUE(controller_0->id());
  EXPECT_EQ(*controller_0->id(), 1u);

  ASSERT_TRUE(controller_0->targets());
  ASSERT_EQ(controller_0->targets()->size(), 2u);

  const std::optional<fuchsia_hardware_spmi::TargetInfo> target_0 =
      FindTargetById(0, *controller_0);
  ASSERT_TRUE(target_0);

  ASSERT_TRUE(target_0->sub_targets());
  ASSERT_EQ(target_0->sub_targets()->size(), 4u);

  const std::optional<fuchsia_hardware_spmi::SubTargetInfo> sub_target_1000 =
      FindSubTargetByAddress(0x1000, *target_0);
  ASSERT_TRUE(sub_target_1000);

  ASSERT_TRUE(sub_target_1000->size());
  EXPECT_EQ(sub_target_1000->size(), 0x1000);

  const std::optional<fuchsia_hardware_spmi::SubTargetInfo> sub_target_2000 =
      FindSubTargetByAddress(0x2000, *target_0);
  ASSERT_TRUE(sub_target_2000);

  ASSERT_TRUE(sub_target_2000->size());
  EXPECT_EQ(sub_target_2000->size(), 0x800);

  const std::optional<fuchsia_hardware_spmi::SubTargetInfo> sub_target_3000 =
      FindSubTargetByAddress(0x3000, *target_0);
  ASSERT_TRUE(sub_target_3000);

  ASSERT_TRUE(sub_target_3000->size());
  EXPECT_EQ(sub_target_3000->size(), 0x400);

  ASSERT_TRUE(sub_target_3000->name());
  EXPECT_EQ(*sub_target_3000->name(), "i2c-core");

  const std::optional<fuchsia_hardware_spmi::SubTargetInfo> sub_target_ffff =
      FindSubTargetByAddress(0xffff, *target_0);
  ASSERT_TRUE(sub_target_ffff);

  ASSERT_TRUE(sub_target_ffff->size());
  ASSERT_EQ(sub_target_ffff->size(), 1);

  ASSERT_TRUE(sub_target_ffff->name());
  EXPECT_EQ(*sub_target_ffff->name(), "i2c-config");

  const std::optional<fuchsia_hardware_spmi::TargetInfo> target_3 =
      FindTargetById(3, *controller_0);
  ASSERT_TRUE(target_3);

  EXPECT_FALSE(target_3->sub_targets());

  ASSERT_TRUE(target_3->name());
  EXPECT_EQ(*target_3->name(), "vreg");

  // First controller composite node specs
  const auto vreg_1000 = spmi_tester->FindMgrRequest("vreg-1000_group");
  ASSERT_TRUE(vreg_1000);

  ASSERT_TRUE(vreg_1000->parents());
  ASSERT_EQ(vreg_1000->parents()->size(), 2u);

  // Skip the devictree node with the compatible string.
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, 1u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0x1000u),
      },
      (*vreg_1000->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                            bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, "target-a"),
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0x1000u),
      },
      (*vreg_1000->parents())[1].properties(), false));

  const auto gpio_2000 = spmi_tester->FindMgrRequest("gpio-2000_group");
  ASSERT_TRUE(gpio_2000);

  ASSERT_TRUE(gpio_2000->parents());
  ASSERT_EQ(gpio_2000->parents()->size(), 2u);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, 1u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0x2000u),
      },
      (*gpio_2000->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                            bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, "target-a"),
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0x2000u),
      },
      (*gpio_2000->parents())[1].properties(), false));

  const auto i2c_3000 = spmi_tester->FindMgrRequest("i2c-3000_group");
  ASSERT_TRUE(i2c_3000);

  ASSERT_TRUE(i2c_3000->parents());
  ASSERT_EQ(i2c_3000->parents()->size(), 3u);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, 1u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0x3000u),
      },
      (*i2c_3000->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                            bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, "target-a"),
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0x3000u),
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_NAME, "i2c-core"),
      },
      (*i2c_3000->parents())[1].properties(), false));

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, 1u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0xffffu),
      },
      (*i2c_3000->parents())[2].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                            bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, 0u),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, "target-a"),
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, 0xffffu),
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_NAME, "i2c-config"),
      },
      (*i2c_3000->parents())[2].properties(), false));

  const auto target_b_3 = spmi_tester->FindMgrRequest("target-b-3_group");
  ASSERT_TRUE(target_b_3);

  ASSERT_TRUE(target_b_3->parents());
  ASSERT_EQ(target_b_3->parents()->size(), 2u);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::TARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::TARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, 1u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, 3u),
      },
      (*target_b_3->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_spmi::TARGETSERVICE,
                            bind_fuchsia_hardware_spmi::TARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, 3u),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, "vreg"),
      },
      (*target_b_3->parents())[1].properties(), false));

  // Second controller metadata
  const auto pbus_node_1 = spmi_tester->FindPbusNode("spmi-abcf0000");
  ASSERT_TRUE(pbus_node_1);

  ASSERT_TRUE(pbus_node_1->metadata());
  ASSERT_EQ(pbus_node_1->metadata()->size(), 1u);

  ASSERT_TRUE((*pbus_node_1->metadata())[0].type());
  EXPECT_EQ(*(*pbus_node_1->metadata())[0].type(),
            fuchsia_hardware_spmi::kControllerInfoMetadataType);

  ASSERT_TRUE((*pbus_node_1->metadata())[0].data());
  const std::vector<uint8_t>& metadata_1 = *(*pbus_node_1->metadata())[0].data();

  const auto controller_1 = fidl::Unpersist<fuchsia_hardware_spmi::ControllerInfo>(
      {metadata_1.data(), metadata_1.size()});
  ASSERT_TRUE(controller_1.is_ok());

  ASSERT_TRUE(controller_1->id());
  EXPECT_EQ(*controller_1->id(), 2u);

  ASSERT_TRUE(controller_1->targets());
  ASSERT_EQ(controller_1->targets()->size(), 1u);

  const std::optional<fuchsia_hardware_spmi::TargetInfo> target_1_0 =
      FindTargetById(0, *controller_1);
  ASSERT_TRUE(target_1_0);
  EXPECT_FALSE(target_1_0->sub_targets());

  // Second controller composite node specs
  const auto target_c_0 = spmi_tester->FindMgrRequest("target-c-0_group");
  ASSERT_TRUE(target_c_0);

  ASSERT_TRUE(target_c_0->parents());
  ASSERT_EQ(target_c_0->parents()->size(), 2u);

  EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
      {
          fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::TARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::TARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, 2u),
          fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, 0u),
      },
      (*target_c_0->parents())[1].bind_rules(), false));
  EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
      {
          fdf::MakeProperty(bind_fuchsia_hardware_spmi::TARGETSERVICE,
                            bind_fuchsia_hardware_spmi::TARGETSERVICE_ZIRCONTRANSPORT),
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, 0u),
      },
      (*target_c_0->parents())[1].properties(), false));

  // The third pbus node should have been ignored by the SPMI visitor
  const auto pbus_node_ignored = spmi_tester->FindPbusNode("not-spmi-abce0000");
  ASSERT_TRUE(pbus_node_ignored);
  EXPECT_FALSE(pbus_node_ignored->metadata());
}

TEST(SpmiVisitorTest, DuplicateTargetId) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  SpmiVisitorTester* const spmi_tester =
      new SpmiVisitorTester("/pkg/test-data/spmi-duplicate-target-id.dtb");
  ASSERT_TRUE(visitors.RegisterVisitor(std::unique_ptr<SpmiVisitorTester>{spmi_tester}).is_ok());

  EXPECT_FALSE(spmi_tester->manager()->Walk(visitors).is_ok());
}

TEST(SpmiVisitorTest, SubTargetSpmiAddressOutOfRange) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  SpmiVisitorTester* const spmi_tester =
      new SpmiVisitorTester("/pkg/test-data/spmi-sub-target-spmi-address-out-of-range.dtb");
  ASSERT_TRUE(visitors.RegisterVisitor(std::unique_ptr<SpmiVisitorTester>{spmi_tester}).is_ok());

  EXPECT_FALSE(spmi_tester->manager()->Walk(visitors).is_ok());
}

}  // namespace spmi_dt

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../regulator-visitor.h"

#include <fidl/fuchsia.hardware.vreg/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <bind/fuchsia/hardware/vreg/cpp/bind.h>
#include <bind/fuchsia/regulator/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/regulator-test.h"
namespace regulator_visitor_dt {

class RegulatorVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<RegulatorVisitor> {
 public:
  RegulatorVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<RegulatorVisitor>(dtb_path,
                                                                     "RegulatorVisitorTest") {}
};

TEST(RegulatorVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<RegulatorVisitorTester>("/pkg/test-data/regulator.dtb");
  RegulatorVisitorTester* regulator_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, regulator_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(regulator_visitor_tester->DoPublish().is_ok());

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;

  auto node_count = regulator_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
  for (size_t i = 0; i < node_count; i++) {
    auto node = regulator_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
    if (node.name()->find("voltage-regulator") != std::string::npos) {
      node_tested_count++;
      auto metadata = regulator_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result vreg_metadata =
          fidl::Unpersist<fuchsia_hardware_vreg::VregMetadata>(metadata_blob);
      ASSERT_TRUE(vreg_metadata.is_ok());
      EXPECT_EQ(vreg_metadata->name(), REGULATOR_NAME);
      EXPECT_EQ(vreg_metadata->min_voltage_uv(), static_cast<uint32_t>(MIN_VOLTAGE));
      EXPECT_EQ(vreg_metadata->voltage_step_uv(), static_cast<uint32_t>(STEP_VOLTAGE));
      EXPECT_EQ(vreg_metadata->num_steps(),
                static_cast<uint32_t>((MAX_VOLTAGE - MIN_VOLTAGE) / STEP_VOLTAGE) + 1);
    }
  }

  node_count = regulator_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::non_pbus_node_size);

  for (size_t i = 0; i < node_count; i++) {
    auto node = regulator_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::non_pbus_nodes_at, i);

    if (node->args().name()->find("cpu-ctrl") != std::string::npos) {
      node_tested_count++;
      ASSERT_EQ(1lu, regulator_visitor_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = regulator_visitor_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents2().has_value());
      ASSERT_EQ(2lu, mgr_request.parents2()->size());

      // Check for regulator parent node specs. Skip the 1st one as it is either pdev/board device.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty2(bind_fuchsia_hardware_vreg::SERVICE,
                               bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty2(bind_fuchsia_regulator::NAME, REGULATOR_NAME)}},
          (*mgr_request.parents2())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_vreg::SERVICE,
                                     bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule2(bind_fuchsia_regulator::NAME, REGULATOR_NAME)}},
          (*mgr_request.parents2())[1].bind_rules(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 2u);
}

}  // namespace regulator_visitor_dt

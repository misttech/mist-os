// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../pwm-visitor.h"

#include <fidl/fuchsia.hardware.pwm/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/pwm.h"

namespace pwm_visitor_dt {

class PwmVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<PwmVisitor> {
 public:
  PwmVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<PwmVisitor>(dtb_path, "PwmVisitorTest") {}
};

TEST(PwmVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<PwmVisitorTester>("/pkg/test-data/pwm.dtb");
  PwmVisitorTester* pwm_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, pwm_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(pwm_visitor_tester->DoPublish().is_ok());

  auto node_count =
      pwm_visitor_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = pwm_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("pwm-ffffa000") != std::string::npos) {
      auto metadata = pwm_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // PWM Channels metadata.
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result pwm_channels =
          fidl::Unpersist<fuchsia_hardware_pwm::PwmChannelsMetadata>(cpp20::span(metadata_blob));
      ASSERT_TRUE(pwm_channels.is_ok());

      ASSERT_TRUE(pwm_channels->channels());
      ASSERT_EQ(pwm_channels->channels()->size(), 2u);
      EXPECT_EQ((*pwm_channels->channels())[0].id(), static_cast<uint32_t>(PIN1));
      EXPECT_EQ((*pwm_channels->channels())[0].period_ns(), static_cast<uint32_t>(PIN1_PERIOD));
      EXPECT_EQ((*pwm_channels->channels())[0].polarity().value(), true);
      EXPECT_FALSE((*pwm_channels->channels())[0].skip_init());
      EXPECT_EQ((*pwm_channels->channels())[1].id(), static_cast<uint32_t>(PIN2));
      EXPECT_EQ((*pwm_channels->channels())[1].period_ns(), static_cast<uint32_t>(PIN2_PERIOD));
      EXPECT_EQ((*pwm_channels->channels())[1].polarity().value(), true);
      EXPECT_EQ((*pwm_channels->channels())[1].skip_init().value(), true);

      node_tested_count++;
    }

    if (node.name()->find("pwm-ffffb000") != std::string::npos) {
      auto metadata = pwm_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test that there are no metadata properties as this pwm is not referenced by any nodes.
      ASSERT_FALSE(metadata);
      node_tested_count++;
    }

    if (node.name()->find("audio") != std::string::npos) {
      node_tested_count++;
      auto mgr_request = pwm_visitor_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(3lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skip that.
      // Bind rules for PIN1
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_pwm::SERVICE,
                                      bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(PIN1)),
          }},
          (*mgr_request.parents())[1].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{
              fdf::MakeProperty(bind_fuchsia_hardware_pwm::SERVICE,
                                bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia_pwm::PWM_ID_FUNCTION,
                                "fuchsia.pwm.PWM_ID_FUNCTION." + std::string(PIN1_NAME)),
          }},
          (*mgr_request.parents())[1].properties(), false));

      // Bind rules for PIN2
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_pwm::SERVICE,
                                      bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(PIN2)),
          }},
          (*mgr_request.parents())[2].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{
              fdf::MakeProperty(bind_fuchsia_hardware_pwm::SERVICE,
                                bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia_pwm::PWM_ID_FUNCTION,
                                "fuchsia.pwm.PWM_ID_FUNCTION." + std::string(PIN2_NAME)),
          }},
          (*mgr_request.parents())[2].properties(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 3u);
}

}  // namespace pwm_visitor_dt

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../adc-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/adc/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/adc/cpp/bind.h>
#include <bind/fuchsia/hardware/adcimpl/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/adc.h"

namespace adc_dt {

class AdcVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<AdcVisitor> {
 public:
  explicit AdcVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<AdcVisitor>(dtb_path, "AdcVisitorTest") {}
};

TEST(AdcVisitorTester, TestAdcsProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<AdcVisitorTester>("/pkg/test-data/adc.dtb");
  AdcVisitorTester* adc_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, adc_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(adc_tester->DoPublish().is_ok());

  auto node_count =
      adc_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        adc_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("vadc-ffffa000") != std::string::npos) {
      auto metadata = adc_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // Controller metadata.
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result controller_metadata =
          fidl::Unpersist<fuchsia_hardware_adcimpl::Metadata>(metadata_blob);
      ASSERT_TRUE(controller_metadata.is_ok());
      ASSERT_TRUE(controller_metadata->channels());
      ASSERT_EQ(controller_metadata->channels()->size(), 1lu);

      ASSERT_TRUE(controller_metadata->channels()->at(0).idx());
      ASSERT_EQ(*controller_metadata->channels()->at(0).idx(), static_cast<uint32_t>(ADC_CHAN1));
      ASSERT_TRUE(controller_metadata->channels()->at(0).name());
      EXPECT_EQ(strcmp(controller_metadata->channels()->at(0).name()->c_str(), ADC_CHAN1_NAME), 0);

      node_tested_count++;
    }

    if (node.name()->find("adc-ffffb000") != std::string::npos) {
      auto metadata = adc_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // Controller metadata.
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result controller_metadata =
          fidl::Unpersist<fuchsia_hardware_adcimpl::Metadata>(metadata_blob);
      ASSERT_TRUE(controller_metadata.is_ok());
      ASSERT_TRUE(controller_metadata->channels());
      ASSERT_EQ(controller_metadata->channels()->size(), 2lu);

      ASSERT_TRUE(controller_metadata->channels()->at(0).idx());
      ASSERT_EQ(*controller_metadata->channels()->at(0).idx(), static_cast<uint32_t>(ADC_CHAN2));
      ASSERT_TRUE(controller_metadata->channels()->at(0).name());
      EXPECT_EQ(strcmp(controller_metadata->channels()->at(0).name()->c_str(), ADC_CHAN2_NAME), 0);

      ASSERT_TRUE(controller_metadata->channels()->at(1).idx());
      ASSERT_EQ(*controller_metadata->channels()->at(1).idx(), static_cast<uint32_t>(ADC_CHAN3));
      ASSERT_TRUE(controller_metadata->channels()->at(1).name());
      EXPECT_EQ(strcmp(controller_metadata->channels()->at(1).name()->c_str(), ADC_CHAN3_NAME), 0);

      node_tested_count++;
    }
  }

  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        adc_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
    if (node.name()->find("audio") != std::string::npos) {
      node_tested_count++;

      ASSERT_EQ(2lu, adc_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = adc_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(3lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd parent is ADC CHAN1.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_adc::SERVICE,
                              bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_adc::CHANNEL, static_cast<uint32_t>(ADC_CHAN1))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_adc::SERVICE,
                                    bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia_adc::CHANNEL, static_cast<uint32_t>(ADC_CHAN1))}},
          (*mgr_request.parents())[1].bind_rules(), false));

      // 3rd parent is ADC CHAN2.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_adc::SERVICE,
                              bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_adc::CHANNEL, static_cast<uint32_t>(ADC_CHAN2))}},
          (*mgr_request.parents())[2].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_adc::SERVICE,
                                    bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia_adc::CHANNEL, static_cast<uint32_t>(ADC_CHAN2))}},
          (*mgr_request.parents())[2].bind_rules(), false));
    }

    if (node.name()->find("video") != std::string::npos) {
      node_tested_count++;

      ASSERT_EQ(2lu, adc_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = adc_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(2lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd parent is ADC CHAN3.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_adc::SERVICE,
                              bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_adc::CHANNEL, static_cast<uint32_t>(ADC_CHAN3))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_adc::SERVICE,
                                    bind_fuchsia_hardware_adc::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia_adc::CHANNEL, static_cast<uint32_t>(ADC_CHAN3))}},
          (*mgr_request.parents())[1].bind_rules(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 4u);
}

}  // namespace adc_dt

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../clockimpl-visitor.h"

#include <fidl/fuchsia.hardware.clockimpl/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <ddk/metadata/clock.h>
#include <gtest/gtest.h>

#include "dts/clock.h"

namespace clock_impl_dt {

class ClockImplVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<ClockImplVisitor> {
 public:
  ClockImplVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<ClockImplVisitor>(dtb_path,
                                                                     "ClockImplVisitorTest") {}
};

TEST(ClockImplVisitorTest, TestClocksProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<ClockImplVisitorTester>("/pkg/test-data/clock.dtb");
  ClockImplVisitorTester* clock_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, clock_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(clock_tester->DoPublish().is_ok());

  auto node_count =
      clock_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        clock_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("clock-controller-ffffa000") != std::string::npos) {
      auto metadata = clock_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
      ASSERT_EQ(4lu, metadata->size());
#else
      ASSERT_EQ(3lu, metadata->size());
#endif

      // ID metadata
      std::vector<uint8_t> metadata_blob_1 = std::move(*(*metadata)[0].data());
      auto metadata_start = reinterpret_cast<clock_id_t*>(metadata_blob_1.data());
      std::vector<clock_id_t> clock_ids(
          metadata_start, metadata_start + (metadata_blob_1.size() / sizeof(clock_id_t)));
      ASSERT_EQ(clock_ids.size(), 3lu);
      EXPECT_EQ(clock_ids[0].clock_id, static_cast<uint32_t>(CLK_ID1));
      EXPECT_EQ(clock_ids[1].clock_id, static_cast<uint32_t>(CLK_ID2));
      EXPECT_EQ(clock_ids[2].clock_id, static_cast<uint32_t>(CLK_ID6));

      // Init steps metadata (legacy)
      {
        std::vector<uint8_t> metadata_blob_2 = std::move(*(*metadata)[1].data());
        fit::result init_steps =
            fidl::Unpersist<fuchsia_hardware_clockimpl::InitMetadata>(cpp20::span(metadata_blob_2));
        ASSERT_TRUE(init_steps.is_ok());
        // Steps expected - Disable for CLK_ID3, SetInput as CLK_ID5, Enable for CLK_ID3
        ASSERT_EQ(init_steps->steps().size(), 3lu);
        EXPECT_EQ(init_steps->steps()[0].id(), static_cast<uint32_t>(CLK_ID3));
        EXPECT_EQ(init_steps->steps()[0].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kDisable);
        EXPECT_EQ(init_steps->steps()[1].id(), static_cast<uint32_t>(CLK_ID3));
        EXPECT_EQ(init_steps->steps()[1].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kInputIdx);
        EXPECT_EQ(init_steps->steps()[1].call()->input_idx().value(),
                  static_cast<uint32_t>(CLK_ID5));
        EXPECT_EQ(init_steps->steps()[2].id(), static_cast<uint32_t>(CLK_ID3));
        EXPECT_EQ(init_steps->steps()[2].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kEnable);
      }

      // Init steps metadata
      {
        std::vector<uint8_t> metadata_blob_3 = std::move(*(*metadata)[2].data());
        fit::result init_steps =
            fidl::Unpersist<fuchsia_hardware_clockimpl::InitMetadata>(cpp20::span(metadata_blob_3));
        ASSERT_TRUE(init_steps.is_ok());
        // Steps expected - Disable for CLK_ID3, SetInput as CLK_ID5, Enable for CLK_ID3
        ASSERT_EQ(init_steps->steps().size(), 3lu);
        EXPECT_EQ(init_steps->steps()[0].id(), static_cast<uint32_t>(CLK_ID3));
        EXPECT_EQ(init_steps->steps()[0].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kDisable);
        EXPECT_EQ(init_steps->steps()[1].id(), static_cast<uint32_t>(CLK_ID3));
        EXPECT_EQ(init_steps->steps()[1].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kInputIdx);
        EXPECT_EQ(init_steps->steps()[1].call()->input_idx().value(),
                  static_cast<uint32_t>(CLK_ID5));
        EXPECT_EQ(init_steps->steps()[2].id(), static_cast<uint32_t>(CLK_ID3));
        EXPECT_EQ(init_steps->steps()[2].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kEnable);
      }

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
      // Clock ID's metadata
      std::vector<uint8_t> metadata_blob_4 = std::move(*(*metadata)[3].data());
      fit::result clock_ids_metadata =
          fidl::Unpersist<fuchsia_hardware_clockimpl::ClockIdsMetadata>(
              cpp20::span(metadata_blob_4));
      const auto& clock_ids2 = clock_ids_metadata->clock_ids();
      ASSERT_TRUE(clock_ids2.has_value());
      ASSERT_EQ(clock_ids2.value().size(), 3lu);
      EXPECT_EQ(clock_ids2.value()[0], static_cast<uint32_t>(CLK_ID1));
      EXPECT_EQ(clock_ids2.value()[1], static_cast<uint32_t>(CLK_ID2));
      EXPECT_EQ(clock_ids2.value()[2], static_cast<uint32_t>(CLK_ID6));
#endif

      node_tested_count++;
    }

    if (node.name()->find("clock-controller-ffffb000") != std::string::npos) {
      auto metadata = clock_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(2lu, metadata->size());

      // Init steps metadata (legacy)
      {
        std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
        fit::result init_steps =
            fidl::Unpersist<fuchsia_hardware_clockimpl::InitMetadata>(cpp20::span(metadata_blob));
        ASSERT_TRUE(init_steps.is_ok());
        // Steps expected - Disable for CLK_ID4, SetRateHz as CLK_ID4_RATE, Enable for CLK_ID4
        ASSERT_EQ(init_steps->steps().size(), 3lu);
        EXPECT_EQ(init_steps->steps()[0].id(), static_cast<uint32_t>(CLK_ID4));
        EXPECT_EQ(init_steps->steps()[0].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kDisable);
        EXPECT_EQ(init_steps->steps()[1].id(), static_cast<uint32_t>(CLK_ID4));
        EXPECT_EQ(init_steps->steps()[1].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kRateHz);
        EXPECT_EQ(init_steps->steps()[1].call()->rate_hz().value(),
                  static_cast<uint32_t>(CLK_ID4_RATE));
        EXPECT_EQ(init_steps->steps()[2].id(), static_cast<uint32_t>(CLK_ID4));
        EXPECT_EQ(init_steps->steps()[2].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kEnable);
      }

      // Init steps metadata
      {
        std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[1].data());
        fit::result init_steps =
            fidl::Unpersist<fuchsia_hardware_clockimpl::InitMetadata>(cpp20::span(metadata_blob));
        ASSERT_TRUE(init_steps.is_ok());
        // Steps expected - Disable for CLK_ID4, SetRateHz as CLK_ID4_RATE, Enable for CLK_ID4
        ASSERT_EQ(init_steps->steps().size(), 3lu);
        EXPECT_EQ(init_steps->steps()[0].id(), static_cast<uint32_t>(CLK_ID4));
        EXPECT_EQ(init_steps->steps()[0].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kDisable);
        EXPECT_EQ(init_steps->steps()[1].id(), static_cast<uint32_t>(CLK_ID4));
        EXPECT_EQ(init_steps->steps()[1].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kRateHz);
        EXPECT_EQ(init_steps->steps()[1].call()->rate_hz().value(),
                  static_cast<uint32_t>(CLK_ID4_RATE));
        EXPECT_EQ(init_steps->steps()[2].id(), static_cast<uint32_t>(CLK_ID4));
        EXPECT_EQ(init_steps->steps()[2].call()->Which(),
                  fuchsia_hardware_clockimpl::InitCall::Tag::kEnable);
      }

      node_tested_count++;
    }

    if (node.name()->find("video") != std::string::npos) {
      ASSERT_EQ(2lu, clock_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = clock_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(3lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                              bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_clock::FUNCTION,
                              "fuchsia.clock.FUNCTION." + std::string(CLK1_NAME))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                    bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, static_cast<uint32_t>(CLK_ID1))}},
          (*mgr_request.parents())[1].bind_rules(), false));

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                              bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_clock::FUNCTION,
                              "fuchsia.clock.FUNCTION." + std::string(CLK2_NAME))}},
          (*mgr_request.parents())[2].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                    bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, static_cast<uint32_t>(CLK_ID2))}},
          (*mgr_request.parents())[2].bind_rules(), false));

      node_tested_count++;
    }

    if (node.name()->find("audio") != std::string::npos) {
      ASSERT_EQ(2lu, clock_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = clock_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(4lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.

      // 2nd is the clock impl parent.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                              bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT)}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                    bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, static_cast<uint32_t>(CLK_ID6))}},
          (*mgr_request.parents())[1].bind_rules(), false));

      // The rest are init step clock parents.
      for (size_t i = 2; i < 4; i++) {
        EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
            {{
                fdf::MakeProperty(bind_fuchsia::INIT_STEP,
                                  bind_fuchsia_clock::BIND_INIT_STEP_CLOCK),
            }},
            (*mgr_request.parents())[i].properties(), false));
        EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
            {{fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                      bind_fuchsia_clock::BIND_INIT_STEP_CLOCK)}},
            (*mgr_request.parents())[i].bind_rules(), false));
      }
      node_tested_count++;
    }
  }

  ASSERT_EQ(node_tested_count, 4u);
}

}  // namespace clock_impl_dt

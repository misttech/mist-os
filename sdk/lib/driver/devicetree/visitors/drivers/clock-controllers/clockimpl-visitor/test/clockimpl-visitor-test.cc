// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../clockimpl-visitor.h"

#include <fidl/fuchsia.hardware.clockimpl/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
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

  auto tester = std::make_unique<ClockImplVisitorTester>("/pkg/test-data/clock.dtb");
  ClockImplVisitorTester* clock_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, clock_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(clock_tester->DoPublish().is_ok());

  auto node_count =
      clock_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        clock_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("clock-controller") != std::string::npos) {
      auto metadata = clock_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // ID metadata
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      auto metadata_start = reinterpret_cast<clock_id_t*>(metadata_blob.data());
      std::vector<clock_id_t> clock_ids(
          metadata_start, metadata_start + (metadata_blob.size() / sizeof(clock_id_t)));
      ASSERT_EQ(clock_ids.size(), 2lu);
      EXPECT_EQ(clock_ids[0].clock_id, static_cast<uint32_t>(CLK_ID1));
      EXPECT_EQ(clock_ids[1].clock_id, static_cast<uint32_t>(CLK_ID2));

      node_tested_count++;
    }
    if (node.name()->find("video") != std::string::npos) {
      ASSERT_EQ(1lu, clock_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = clock_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, 0);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(3lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
            fdf::MakeProperty(bind_fuchsia_clock::FUNCTION,
                              "fuchsia.clock.FUNCTION." + std::string(CLK1_NAME))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                    bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
            fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, static_cast<uint32_t>(CLK_ID1))}},
          (*mgr_request.parents())[1].bind_rules(), false));

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                              bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
            fdf::MakeProperty(bind_fuchsia_clock::FUNCTION,
                              "fuchsia.clock.FUNCTION." + std::string(CLK2_NAME))}},
          (*mgr_request.parents())[2].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                    bind_fuchsia_clock::BIND_FIDL_PROTOCOL_SERVICE),
            fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID, static_cast<uint32_t>(CLK_ID2))}},
          (*mgr_request.parents())[2].bind_rules(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace clock_impl_dt

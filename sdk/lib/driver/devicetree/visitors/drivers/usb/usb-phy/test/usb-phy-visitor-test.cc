// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../usb-phy-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/usb/phy/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <gtest/gtest.h>
namespace usb_phy_visitor_dt {

class UsbPhyVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<UsbPhyVisitor> {
 public:
  UsbPhyVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<UsbPhyVisitor>(dtb_path, "UsbPhyVisitorTest") {}
};

TEST(UsbVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<UsbPhyVisitorTester>("/pkg/test-data/usb-phy.dtb");
  UsbPhyVisitorTester* usb_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, usb_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(usb_visitor_tester->DoPublish().is_ok());

  auto node_count =
      usb_visitor_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = usb_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("usb") != std::string::npos) {
      node_tested_count++;
      auto mgr_request = usb_visitor_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(2lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skip that.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_usb_phy::SERVICE,
                                    bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                                    bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
            fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                                    bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
            fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                                    bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_XHCI)}},
          (*mgr_request.parents())[1].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{
              fdf::MakeProperty(bind_fuchsia_hardware_usb_phy::SERVICE,
                                bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
              fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                                bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
              fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID,
                                bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
              fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                                bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_XHCI),
          }},
          (*mgr_request.parents())[1].properties(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace usb_phy_visitor_dt

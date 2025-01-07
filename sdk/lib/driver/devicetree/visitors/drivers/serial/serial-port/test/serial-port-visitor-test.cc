// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../serial-port-visitor.h"

#include <fidl/fuchsia.hardware.serial/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/serial/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/serial-port-test.h"
namespace serial_port_visitor_dt {

class SerialPortVisitorTester
    : public fdf_devicetree::testing::VisitorTestHelper<SerialPortVisitor> {
 public:
  SerialPortVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<SerialPortVisitor>(dtb_path,
                                                                      "SerialPortVisitorTest") {}
};

TEST(SerialPortVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<SerialPortVisitorTester>("/pkg/test-data/serial-port.dtb");
  SerialPortVisitorTester* serial_port_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, serial_port_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(serial_port_visitor_tester->DoPublish().is_ok());

  auto node_count = serial_port_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = serial_port_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("bt-uart") != std::string::npos) {
      node_tested_count++;
      auto metadata = serial_port_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result serial_port =
          fidl::Unpersist<fuchsia_hardware_serial::SerialPortInfo>(metadata_blob);
      ASSERT_TRUE(serial_port.is_ok());
      EXPECT_EQ(serial_port->serial_class(),
                static_cast<fuchsia_hardware_serial::Class>(TEST_CLASS));
      EXPECT_EQ(serial_port->serial_vid(), static_cast<uint32_t>(TEST_VID));
      EXPECT_EQ(serial_port->serial_pid(), static_cast<uint32_t>(TEST_PID));
    }

    if (node.name()->find("bt") != std::string::npos) {
      node_tested_count++;
      ASSERT_EQ(1lu, serial_port_visitor_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = serial_port_visitor_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(2lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd parent is bt-uart.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_serial::BIND_PROTOCOL_DEVICE),
            fdf::MakeProperty(bind_fuchsia_serial::NAME, TEST_NAME)}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                                    bind_fuchsia_serial::BIND_PROTOCOL_DEVICE),
            fdf::MakeAcceptBindRule(bind_fuchsia::SERIAL_CLASS,
                                    static_cast<uint32_t>(TEST_CLASS))}},
          (*mgr_request.parents())[1].bind_rules(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 2u);
}

}  // namespace serial_port_visitor_dt

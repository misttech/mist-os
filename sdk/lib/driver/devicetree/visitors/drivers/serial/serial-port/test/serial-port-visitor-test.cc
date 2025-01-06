// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../serial-port-visitor.h"

#include <fidl/fuchsia.hardware.serial/cpp/fidl.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

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
      ASSERT_EQ(2lu, metadata->size());

      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result serial_port =
          fidl::Unpersist<fuchsia_hardware_serial::SerialPortInfo>(metadata_blob);
      ASSERT_TRUE(serial_port.is_ok());
      EXPECT_EQ(serial_port->serial_class(),
                static_cast<fuchsia_hardware_serial::Class>(TEST_CLASS));
      EXPECT_EQ(serial_port->serial_vid(), static_cast<uint32_t>(TEST_VID));
      EXPECT_EQ(serial_port->serial_pid(), static_cast<uint32_t>(TEST_PID));

      std::vector<uint8_t> metadata_blob2 = std::move(*(*metadata)[1].data());
      fit::result serial_port2 =
          fidl::Unpersist<fuchsia_hardware_serial::SerialPortInfo>(metadata_blob2);
      ASSERT_TRUE(serial_port2.is_ok());
      EXPECT_EQ(serial_port2->serial_class(),
                static_cast<fuchsia_hardware_serial::Class>(TEST_CLASS));
      EXPECT_EQ(serial_port2->serial_vid(), static_cast<uint32_t>(TEST_VID));
      EXPECT_EQ(serial_port2->serial_pid(), static_cast<uint32_t>(TEST_PID));
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace serial_port_visitor_dt

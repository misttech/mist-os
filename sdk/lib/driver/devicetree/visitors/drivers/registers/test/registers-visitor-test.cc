// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../registers-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/registers/cpp/bind.h>
#include <bind/fuchsia/register/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/registers.h"

namespace registers_dt {

class RegistersVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<RegistersVisitor> {
 public:
  RegistersVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<RegistersVisitor>(dtb_path,
                                                                     "RegistersVisitorTest") {}
};

TEST(RegistersVisitorTest, TestRegistersProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<RegistersVisitorTester>("/pkg/test-data/registers.dtb");
  RegistersVisitorTester* registers_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, registers_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(registers_tester->DoPublish().is_ok());

  auto node_count =
      registers_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = registers_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("register-controller-ffffa000") != std::string::npos) {
      node_tested_count++;
      auto metadata = registers_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result decoded =
          fidl::Unpersist<fuchsia_hardware_registers::Metadata>(cpp20::span(metadata_blob));
      ASSERT_TRUE(decoded.is_ok());
      ASSERT_TRUE((*decoded).registers());
      auto& registers = *(*decoded).registers();
      ASSERT_EQ(registers.size(), 2u);
      uint32_t registers_verified = 0;
      for (auto reg : registers) {
        if (reg.name() == "usb-0") {
          registers_verified++;
          ASSERT_TRUE(reg.masks());
          auto& masks = *reg.masks();
          ASSERT_EQ(masks.size(), 2u);
          EXPECT_EQ(masks[0].mmio_offset(), REGISTER_OFFSET1);
          EXPECT_EQ(masks[1].mmio_offset(), REGISTER_OFFSET2);
          EXPECT_EQ(masks[0].count(), 1);
          EXPECT_EQ(masks[1].count(), 1);
          // REGISTER_LENGTH1 is 1 byte. Therefore mask will be R8.
          EXPECT_EQ(masks[0].mask()->r8().value(), static_cast<uint8_t>(REGISTER_MASK1));
          // REGISTER_LENGTH2 is 8 bytes. Therefore mask will be R64.
          uint64_t mask2 = static_cast<uint32_t>(REGISTER_MASK2_0) |
                           (static_cast<uint64_t>(REGISTER_MASK2_1) << 32);
          EXPECT_EQ(masks[1].mask()->r64().value(), mask2);
          EXPECT_EQ(masks[0].overlap_check_on(), true);
          EXPECT_EQ(masks[1].overlap_check_on(), true);

        } else if (reg.name() == REGISTER_NAME3) {
          registers_verified++;
          ASSERT_TRUE(reg.masks());
          auto& masks = *reg.masks();
          ASSERT_EQ(masks.size(), 1u);
          EXPECT_EQ(masks[0].mmio_offset(), REGISTER_OFFSET3);
          EXPECT_EQ(masks[0].count(), 1);
          // REGISTER_LENGTH3 is 4 bytes. Therefore mask will be R32.
          EXPECT_EQ(masks[0].mask()->r32().value(), REGISTER_MASK3);
          EXPECT_EQ(masks[0].overlap_check_on(), true);
        }
      }

      EXPECT_EQ(registers_verified, registers.size());

    } else if (node.name()->find("register-controller-ffffb000") != std::string::npos) {
      node_tested_count++;
      auto metadata = registers_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result decoded =
          fidl::Unpersist<fuchsia_hardware_registers::Metadata>(cpp20::span(metadata_blob));
      ASSERT_TRUE(decoded.is_ok());
      ASSERT_TRUE((*decoded).registers());
      auto& registers = *(*decoded).registers();

      ASSERT_EQ(registers.size(), 1u);
      ASSERT_TRUE(registers[0].masks());
      auto& masks = *registers[0].masks();
      ASSERT_EQ(masks.size(), 1u);
      EXPECT_EQ(masks[0].mmio_offset(), REGISTER_OFFSET4);
      EXPECT_EQ(masks[0].count(), 1);
      // REGISTER_LENGTH4 is 2 bytes. Therefore mask will be R16.
      EXPECT_EQ(masks[0].mask()->r16().value(), REGISTER_MASK4);
      EXPECT_EQ(masks[0].overlap_check_on(), false);

    } else if (node.name()->find("usb-0") != std::string::npos) {
      node_tested_count++;
      auto mgr_request = registers_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx);
      mgr_request_idx++;
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_registers::SERVICE,
                                     bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule2(bind_fuchsia_register::NAME, node.name()->c_str())}},
          (*mgr_request.parents2())[1].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{
              fdf::MakeProperty2(bind_fuchsia_hardware_registers::SERVICE,
                                 bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
          }},
          (*mgr_request.parents2())[1].properties(), false));

    } else if (node.name()->find("usb-100") != std::string::npos) {
      node_tested_count++;
      auto mgr_request = registers_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx);
      mgr_request_idx++;
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_registers::SERVICE,
                                     bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule2(bind_fuchsia_register::NAME, REGISTER_NAME3)}},

          (*mgr_request.parents2())[1].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty2(bind_fuchsia_hardware_registers::SERVICE,
                               bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty2(bind_fuchsia_register::NAME, REGISTER_NAME3)}},
          (*mgr_request.parents2())[1].properties(), false));

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_registers::SERVICE,
                                     bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule2(bind_fuchsia_register::NAME, REGISTER_NAME4)}},

          (*mgr_request.parents2())[2].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty2(bind_fuchsia_hardware_registers::SERVICE,
                               bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty2(bind_fuchsia_register::NAME, REGISTER_NAME4)}},
          (*mgr_request.parents2())[2].properties(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 4u);
}

}  // namespace registers_dt

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <gtest/gtest.h>

#include "../arm-gicv2-visitor.h"
#include "dts/interrupts.h"

namespace arm_gic_dt {
namespace {

class ArmGicV2VisitorTester : public fdf_devicetree::testing::VisitorTestHelper<ArmGicV2Visitor> {
 public:
  explicit ArmGicV2VisitorTester(std::string_view dtb_path)
      : VisitorTestHelper<ArmGicV2Visitor>(dtb_path, "ArmGicV2VisitorTest") {}
};

TEST(ArmGicV2VisitorTest, TestInterruptProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<ArmGicV2VisitorTester>("/pkg/test-data/interrupts.dtb");
  ArmGicV2VisitorTester* irq_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, irq_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(irq_tester->DoPublish().is_ok());

  auto node_count =
      irq_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        irq_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name() == "sample-device-1") {
      auto irq = irq_tester->env()
                     .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                     .irq();
      ASSERT_TRUE(irq);
      ASSERT_EQ(2lu, irq->size());
      EXPECT_EQ(static_cast<uint32_t>(IRQ1_SPI) + 32, *(*irq)[0].irq());
      EXPECT_EQ(static_cast<uint32_t>(IRQ2_PPI) + 16, *(*irq)[1].irq());
      EXPECT_EQ(static_cast<uint32_t>(IRQ1_MODE_FUCHSIA), *(*irq)[0].mode());
      EXPECT_EQ(static_cast<uint32_t>(IRQ2_MODE_FUCHSIA), *(*irq)[1].mode());

      node_tested_count++;
    }

    if (node.name() == "sample-device-2") {
      auto irq = irq_tester->env()
                     .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                     .irq();
      ASSERT_TRUE(irq);
      ASSERT_EQ(2lu, irq->size());
      EXPECT_EQ(static_cast<uint32_t>(IRQ3_SPI) + 32, *(*irq)[0].irq());
      EXPECT_EQ(static_cast<uint32_t>(IRQ4_PPI) + 16, *(*irq)[1].irq());
      EXPECT_EQ(static_cast<uint32_t>(IRQ3_MODE_FUCHSIA), *(*irq)[0].mode());
      EXPECT_EQ(static_cast<uint32_t>(IRQ4_MODE_FUCHSIA), *(*irq)[1].mode());

      node_tested_count++;
    }
  }

  ASSERT_EQ(node_tested_count, 2u);
}

}  // namespace
}  // namespace arm_gic_dt

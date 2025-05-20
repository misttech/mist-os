// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>
#include <zircon/assert.h>

#include <cstdint>
#include <memory>
#include <string_view>

#include <gtest/gtest.h>

#include "../arm-gic-visitor.h"
#include "dts/interrupts.h"

namespace arm_gic_dt {
namespace {

using ArmGicVisitorType = fdf_devicetree::testing::VisitorTestHelper<ArmGicVisitor>;
class ArmGicVisitorTester : public ArmGicVisitorType {
 public:
  explicit ArmGicVisitorTester(std::string_view dtb_path)
      : ArmGicVisitorType(dtb_path, "ArmGicV2VisitorTest") {}
};

class ArmGicVisitorTest : public testing::Test {
 protected:
  void SetUp() final {
    visitors_ = std::make_unique<fdf_devicetree::VisitorRegistry>();
    ASSERT_TRUE(visitors_->RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>())
                    .is_ok());

    auto tester = std::make_unique<ArmGicVisitorTester>("/pkg/test-data/interrupts.dtb");
    irq_tester_ = tester.get();
    ASSERT_TRUE(visitors_->RegisterVisitor(std::move(tester)).is_ok());

    ASSERT_EQ(ZX_OK, irq_tester_->manager()->Walk(*visitors_).status_value());
    ASSERT_TRUE(irq_tester_->DoPublish().is_ok());

    for (size_t i = 0; i < NodeCount(); i++) {
      auto node = NodeAt(i);
      if (node.name().has_value()) {
        nodes_[node.name().value()] = node;
      }
    }
  }

  size_t NodeCount() {
    return irq_tester_->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
  }

  fuchsia_hardware_platform_bus::Node& NodeByName(const std::string& name) {
    return nodes_.at(name);
  }

  fuchsia_hardware_platform_bus::Node NodeAt(size_t i) {
    return irq_tester_->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
  }

 private:
  std::unique_ptr<fdf_devicetree::VisitorRegistry> visitors_;
  ArmGicVisitorTester* irq_tester_ = nullptr;
  std::unordered_map<std::string, fuchsia_hardware_platform_bus::Node> nodes_;
};

TEST_F(ArmGicVisitorTest, TestInterruptProperty1) {
  auto irq = NodeByName("sample-device-1").irq();
  ASSERT_TRUE(irq);
  ASSERT_EQ(2lu, irq->size());
  EXPECT_EQ(static_cast<uint32_t>(IRQ1_SPI) + 32, *(*irq)[0].irq());
  EXPECT_EQ(static_cast<uint32_t>(IRQ2_PPI) + 16, *(*irq)[1].irq());
  EXPECT_EQ(static_cast<uint32_t>(IRQ1_MODE_FUCHSIA), static_cast<uint32_t>(*(*irq)[0].mode()));
  EXPECT_EQ(static_cast<uint32_t>(IRQ2_MODE_FUCHSIA), static_cast<uint32_t>(*(*irq)[1].mode()));
  ASSERT_TRUE((*irq)[0].name().has_value());
  EXPECT_EQ("interrupt-first", *(*irq)[0].name());
  ASSERT_TRUE((*irq)[1].name().has_value());
  EXPECT_EQ("interrupt-second", *(*irq)[1].name());
}

TEST_F(ArmGicVisitorTest, TestInterruptProperty2) {
  auto irq = NodeByName("sample-device-2").irq();
  ASSERT_TRUE(irq);
  ASSERT_EQ(2lu, irq->size());
  EXPECT_EQ(static_cast<uint32_t>(IRQ3_SPI) + 32, *(*irq)[0].irq());
  EXPECT_EQ(static_cast<uint32_t>(IRQ4_PPI) + 16, *(*irq)[1].irq());
  EXPECT_EQ(static_cast<uint32_t>(IRQ3_MODE_FUCHSIA), static_cast<uint32_t>(*(*irq)[0].mode()));
  EXPECT_EQ(static_cast<uint32_t>(IRQ4_MODE_FUCHSIA), static_cast<uint32_t>(*(*irq)[1].mode()));
  ASSERT_TRUE((*irq)[0].name().has_value());
  EXPECT_EQ("interrupt-first", *(*irq)[0].name());
  ASSERT_TRUE((*irq)[1].name().has_value());
  EXPECT_EQ("interrupt-second", *(*irq)[1].name());
}

TEST_F(ArmGicVisitorTest, TestInterruptProperty3) {
  auto irq = NodeByName("sample-device-3").irq();
  ASSERT_TRUE(irq);
  ASSERT_EQ(2lu, irq->size());
  EXPECT_EQ(static_cast<uint32_t>(IRQ5_SPI) + 32, *(*irq)[0].irq());
  EXPECT_EQ(static_cast<uint32_t>(IRQ6_SPI) + 32, *(*irq)[1].irq());
  EXPECT_EQ(static_cast<uint32_t>(IRQ5_MODE_FUCHSIA), static_cast<uint32_t>(*(*irq)[0].mode()));
  EXPECT_EQ(static_cast<uint32_t>(IRQ6_MODE_FUCHSIA), static_cast<uint32_t>(*(*irq)[1].mode()));
  ASSERT_FALSE((*irq)[0].name().has_value());
  ASSERT_FALSE((*irq)[1].name().has_value());
}

}  // namespace
}  // namespace arm_gic_dt

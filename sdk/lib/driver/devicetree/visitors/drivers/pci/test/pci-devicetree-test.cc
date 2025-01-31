// Copyright 2025 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <gtest/gtest.h>

#include "sdk/lib/driver/devicetree/visitors/drivers/pci/pci.h"
#include "src/lib/testing/predicates/status.h"

namespace pci_dt {

namespace {

class PciVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<PciVisitor> {
 public:
  PciVisitorTester(std::string_view dtb_path)
      : VisitorTestHelper<PciVisitor>(dtb_path, "CrosvmVisitorTest") {}
};

TEST(PciVisitorTest, CrosvmArm64) {
  fdf_devicetree::VisitorRegistry visitors;

  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<PciVisitorTester>("/pkg/test-data/crosvm_arm64_pci_golden.dtb");
  auto* pci_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_OK(pci_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(pci_tester->has_visited());

  auto reg = pci_tester->reg();
  ASSERT_TRUE(reg);
  uint64_t address = *reg->address();
  EXPECT_EQ(address, 0x72000000ull) << std::hex << address;
  uint64_t size = *reg->size();
  EXPECT_EQ(size, 0x1000000ull) << std::hex << size;

  auto ranges = pci_tester->ranges();
  ASSERT_EQ(ranges.size(), 2u);
  auto first_range = ranges[0].range;
  EXPECT_EQ(*first_range.child_bus_address(), 0x70000000ull)
      << std::hex << *first_range.child_bus_address();
  EXPECT_EQ(*first_range.parent_bus_address(), 0x70000000ull)
      << std::hex << *first_range.parent_bus_address();
  EXPECT_EQ(*first_range.length(), 0x2000000ull) << std::hex << *first_range.length();
  EXPECT_EQ(ranges[0].bus_address_high_cell, 0x3000000ull)
      << std::hex << ranges[0].bus_address_high_cell;

  auto second_range = ranges[1].range;
  EXPECT_EQ(*second_range.child_bus_address(), 0x91600000ull)
      << std::hex << *second_range.child_bus_address();
  EXPECT_EQ(*second_range.parent_bus_address(), 0x91600000ull)
      << std::hex << *second_range.parent_bus_address();
  EXPECT_EQ(*second_range.length(), 0xff6ea00000ull) << std::hex << *second_range.length();
  EXPECT_EQ(ranges[1].bus_address_high_cell, 0x43000000ull)
      << std::hex << ranges[1].bus_address_high_cell;
}

TEST(PciVisitorTest, QemuArm64) {
  fdf_devicetree::VisitorRegistry visitors;

  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<PciVisitorTester>("/pkg/test-data/qemu_arm64_pci_golden.dtb");
  auto* pci_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_OK(pci_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(pci_tester->has_visited());

  auto reg = pci_tester->reg();
  ASSERT_TRUE(reg);
  uint64_t address = *reg->address();
  EXPECT_EQ(address, 0x3f000000ull) << std::hex << address;
  uint64_t size = *reg->size();
  EXPECT_EQ(size, 0x1000000ull) << std::hex << size;

  auto ranges = pci_tester->ranges();
  ASSERT_EQ(ranges.size(), 3u);

  // First range:
  // | child bus           | parent bus      | size          |
  // | 0x1000000 0x00 0x00 | 0x00 0x3eff0000 |  0x00 0x10000 |
  auto first_range = ranges[0].range;
  EXPECT_EQ(*first_range.child_bus_address(), 0x0ull)
      << std::hex << *first_range.child_bus_address();
  EXPECT_EQ(*first_range.parent_bus_address(), 0x3eff0000ull)
      << std::hex << *first_range.parent_bus_address();
  EXPECT_EQ(*first_range.length(), 0x10000ull) << std::hex << *first_range.length();
  EXPECT_EQ(ranges[0].bus_address_high_cell, 0x1000000ull)
      << std::hex << ranges[0].bus_address_high_cell;

  // Second range:
  // | child bus                 | parent bus      | size            |
  // | 0x2000000 0x00 0x10000000 | 0x00 0x10000000 | 0x00 0x2eff0000 |
  auto second_range = ranges[1].range;
  EXPECT_EQ(*second_range.child_bus_address(), 0x10000000ull)
      << std::hex << *second_range.child_bus_address();
  EXPECT_EQ(*second_range.parent_bus_address(), 0x10000000ull)
      << std::hex << *second_range.parent_bus_address();
  EXPECT_EQ(*second_range.length(), 0x2eff0000ull) << std::hex << *second_range.length();
  EXPECT_EQ(ranges[1].bus_address_high_cell, 0x2000000ull)
      << std::hex << ranges[1].bus_address_high_cell;

  // Third range:
  // | child bus           | parent bus | size      |
  // | 0x3000000 0x80 0x00 | 0x80 0x00  | 0x80 0x00 |
  auto third_range = ranges[2].range;
  EXPECT_EQ(*third_range.child_bus_address(), 0x8000000000ull)
      << std::hex << *third_range.child_bus_address();
  EXPECT_EQ(*third_range.parent_bus_address(), 0x8000000000ull)
      << std::hex << *third_range.parent_bus_address();
  EXPECT_EQ(*third_range.length(), 0x8000000000ull) << std::hex << *third_range.length();
  EXPECT_EQ(ranges[2].bus_address_high_cell, 0x3000000ull)
      << std::hex << ranges[2].bus_address_high_cell;
}

}  // namespace
}  // namespace pci_dt

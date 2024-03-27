// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../mmio.h"

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <gtest/gtest.h>

#include "dts/reg.h"

namespace fdf_devicetree {
namespace {

class MmioVisitorTester : public testing::VisitorTestHelper<MmioVisitor> {
 public:
  MmioVisitorTester(std::string_view dtb_path)
      : VisitorTestHelper<MmioVisitor>(dtb_path, "MmioVisitorTest") {}
};

TEST(MmioVisitorTest, ReadRegSuccessfully) {
  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<MmioVisitorTester>("/pkg/test-data/mmio.dtb");
  MmioVisitorTester* mmio_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());
  ASSERT_TRUE(mmio_tester->manager()->Walk(visitors).is_ok());
  ASSERT_TRUE(mmio_tester->has_visited());
  ASSERT_TRUE(mmio_tester->DoPublish().is_ok());

  // Check MMIO of sample-device.
  auto mmio = mmio_tester->env()
                  .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, 0)
                  .mmio();

  // Test MMIO properties.
  ASSERT_TRUE(mmio);
  ASSERT_EQ(3lu, mmio->size());
  ASSERT_EQ(REG_A_BASE, *(*mmio)[0].base());
  ASSERT_EQ(static_cast<uint64_t>(REG_A_LENGTH), *(*mmio)[0].length());
  ASSERT_EQ((uint64_t)REG_B_BASE_WORD0 << 32 | REG_B_BASE_WORD1, *(*mmio)[1].base());
  ASSERT_EQ((uint64_t)REG_B_LENGTH_WORD0 << 32 | REG_B_LENGTH_WORD1, *(*mmio)[1].length());
  ASSERT_EQ((uint64_t)REG_C_BASE_WORD0 << 32 | REG_C_BASE_WORD1, *(*mmio)[2].base());
  ASSERT_EQ((uint64_t)REG_C_LENGTH_WORD0 << 32 | REG_C_LENGTH_WORD1, *(*mmio)[2].length());
}

TEST(MmioVisitorTest, TranslateRegSuccessfully) {
  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<MmioVisitorTester>("/pkg/test-data/ranges.dtb");
  MmioVisitorTester* mmio_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());
  ASSERT_TRUE(mmio_tester->manager()->Walk(visitors).is_ok());
  ASSERT_TRUE(mmio_tester->has_visited());
  ASSERT_TRUE(mmio_tester->DoPublish().is_ok());

  auto parent_mmio = mmio_tester->env()
                         .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, 0)
                         .mmio();
  auto child_mmio = mmio_tester->env()
                        .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, 1)
                        .mmio();

  ASSERT_TRUE(parent_mmio);
  ASSERT_TRUE(child_mmio);
  ASSERT_EQ(1lu, parent_mmio->size());
  ASSERT_EQ(1lu, child_mmio->size());
  ASSERT_EQ(RANGE_BASE, *(*parent_mmio)[0].base());
  ASSERT_EQ(RANGE_OFFSET + RANGE_BASE, *(*child_mmio)[0].base());
  ASSERT_EQ((uint64_t)RANGE_SIZE, *(*parent_mmio)[0].length());
  ASSERT_EQ((uint64_t)RANGE_OFFSET_SIZE, *(*child_mmio)[0].length());
}

TEST(MmioVisitorTest, IgnoreRegWhichIsNotMmio) {
  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<MmioVisitorTester>("/pkg/test-data/not-mmio.dtb");
  MmioVisitorTester* mmio_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());
  ASSERT_TRUE(mmio_tester->manager()->Walk(visitors).is_ok());
  ASSERT_TRUE(mmio_tester->has_visited());
  ASSERT_TRUE(mmio_tester->DoPublish().is_ok());
  ASSERT_EQ(0lu, mmio_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size));
  ASSERT_EQ(3lu, mmio_tester->env().SyncCall(&testing::FakeEnvWrapper::non_pbus_node_size));
}

}  // namespace
}  // namespace fdf_devicetree

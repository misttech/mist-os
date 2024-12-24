// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../bti.h"

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <gtest/gtest.h>

#include "dts/iommu.h"

namespace fdf_devicetree {
namespace {

class BtiVisitorTester : public testing::VisitorTestHelper<BtiVisitor> {
 public:
  BtiVisitorTester(std::string_view dtb_path)
      : VisitorTestHelper<BtiVisitor>(dtb_path, "BtiVisitorTest") {}
};

TEST(BtiVisitorTest, TestBtiProperty) {
  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<BtiVisitorTester>("/pkg/test-data/iommu.dtb");
  BtiVisitorTester* bti_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, bti_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(bti_tester->DoPublish().is_ok());

  auto node_count = bti_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = bti_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name() == "sample-bti-device1") {
      auto bti1 = bti_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, i).bti();

      // Test BTI properties.
      ASSERT_TRUE(bti1);
      ASSERT_EQ(1lu, bti1->size());
      // iommu index is 0, as there is only one iommu provider.
      ASSERT_EQ(0u, *(*bti1)[0].iommu_index());
      ASSERT_EQ((uint32_t)TEST_BTI_ID1, *(*bti1)[0].bti_id());

      node_tested_count++;
    }

    if (node.name() == "sample-bti-device2") {
      auto bti2 = bti_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, i).bti();

      // Test BTI properties.
      ASSERT_TRUE(bti2);
      ASSERT_EQ(1lu, bti2->size());
      // iommu index is 0, as there is only one iommu provider.
      ASSERT_EQ(0u, *(*bti2)[0].iommu_index());
      ASSERT_EQ((uint32_t)TEST_BTI_ID2, *(*bti2)[0].bti_id());
      ASSERT_EQ(TEST_BTI_ID2_NAME, *(*bti2)[0].name());

      node_tested_count++;
    }
  }

  ASSERT_EQ(node_tested_count, 2u);
}

}  // namespace
}  // namespace fdf_devicetree

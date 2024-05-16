// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/smc/smc.h>
#include <lib/driver/devicetree/visitors/default/smc/test/dts/smc-test.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <gtest/gtest.h>

namespace fdf_devicetree {
namespace {

class SmcVisitorTester : public testing::VisitorTestHelper<SmcVisitor> {
 public:
  SmcVisitorTester(std::string_view dtb_path)
      : VisitorTestHelper<SmcVisitor>(dtb_path, "SmcVisitorTest") {}
};

TEST(SmcVisitorTest, TestSmcProperty) {
  VisitorRegistry visitors;
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<SmcVisitorTester>("/pkg/test-data/smc.dtb");
  SmcVisitorTester* smc_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, smc_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(smc_tester->DoPublish().is_ok());

  auto node_count = smc_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = smc_tester->env().SyncCall(&testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name() == "sample-device") {
      auto smc = node.smc();

      // Test smc properties.
      ASSERT_TRUE(smc);
      ASSERT_EQ(1lu, smc->size());
      EXPECT_EQ(*(*smc)[0].service_call_num_base(), static_cast<uint64_t>(TEST_SMC_BASE));
      EXPECT_EQ(*(*smc)[0].count(), static_cast<uint64_t>(TEST_SMC_COUNT));
      EXPECT_EQ(*(*smc)[0].exclusive(), static_cast<uint64_t>(TEST_SMC_EXCLUSIVE_FLAG));
      EXPECT_EQ(*(*smc)[0].name(), TEST_SMC_NAME);

      node_tested_count++;
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace
}  // namespace fdf_devicetree

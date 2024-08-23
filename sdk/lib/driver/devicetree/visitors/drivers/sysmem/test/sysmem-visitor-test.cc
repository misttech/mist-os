// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../sysmem-visitor.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <gtest/gtest.h>

#include "dts/sysmem-test.h"

namespace sysmem_dt {

class SysmemVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<SysmemVisitor> {
 public:
  SysmemVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<SysmemVisitor>(dtb_path, "SysmemVisitorTest") {}
};

TEST(SysmemVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<SysmemVisitorTester>("/pkg/test-data/sysmem.dtb");
  SysmemVisitorTester* sysmem_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, sysmem_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(sysmem_visitor_tester->DoPublish().is_ok());

  uint32_t node_tested_count = 0;

  auto node_count = sysmem_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);
  for (size_t i = 0; i < node_count; i++) {
    auto node = sysmem_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
    if (node.name()->find("fuchsia-sysmem") != std::string::npos) {
      node_tested_count++;
      auto metadata = sysmem_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result sysmem_metadata =
          fidl::Unpersist<fuchsia_hardware_sysmem::Metadata>(metadata_blob);
      ASSERT_TRUE(sysmem_metadata.is_ok());
      EXPECT_EQ(sysmem_metadata->vid(), static_cast<uint32_t>(TEST_VID));
      EXPECT_EQ(sysmem_metadata->pid(), static_cast<uint32_t>(TEST_PID));
      EXPECT_EQ(sysmem_metadata->contiguous_memory_size(),
                static_cast<uint32_t>(TEST_CONTIGUOUS_SIZE));
      EXPECT_EQ(sysmem_metadata->protected_memory_size(),
                static_cast<uint32_t>(TEST_PROTECTED_SIZE));
    }
  }
  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace sysmem_dt

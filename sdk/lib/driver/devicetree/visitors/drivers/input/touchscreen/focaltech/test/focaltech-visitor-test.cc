// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../focaltech-visitor.h"

#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>
#include <lib/focaltech/focaltech.h>

#include <cstdint>

#include <gtest/gtest.h>
namespace focaltech_visitor_dt {

class FocaltechVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<FocaltechVisitor> {
 public:
  FocaltechVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<FocaltechVisitor>(dtb_path,
                                                                     "FocaltechVisitorTest") {}
};

TEST(FocaltechVisitorTest, TestMetadataAndBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<FocaltechVisitorTester>("/pkg/test-data/focaltech.dtb");
  FocaltechVisitorTester* focaltech_visitor_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, focaltech_visitor_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(focaltech_visitor_tester->DoPublish().is_ok());

  auto node_count = focaltech_visitor_tester->env().SyncCall(
      &fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node = focaltech_visitor_tester->env().SyncCall(
        &fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("touchscreen") != std::string::npos) {
      node_tested_count++;
      auto metadata = focaltech_visitor_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      auto device_info = reinterpret_cast<FocaltechMetadata*>(metadata_blob.data());
      EXPECT_EQ(device_info->device_id, static_cast<uint32_t>(FOCALTECH_DEVICE_FT6336));
      EXPECT_TRUE(device_info->needs_firmware);
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace focaltech_visitor_dt

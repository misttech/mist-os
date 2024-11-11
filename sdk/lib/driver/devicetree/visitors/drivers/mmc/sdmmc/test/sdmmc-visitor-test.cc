// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../sdmmc-visitor.h"

#include <fidl/fuchsia.hardware.sdmmc/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/sdmmc.h"
#include "fidl/fuchsia.hardware.sdmmc/cpp/natural_types.h"

namespace sdmmc_dt {

class SdmmcVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<SdmmcVisitor> {
 public:
  SdmmcVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<SdmmcVisitor>(dtb_path, "SdmmcVisitorTest") {}
};

TEST(SdmmcVisitorTest, TestClocksProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<SdmmcVisitorTester>("/pkg/test-data/sdmmc.dtb");
  SdmmcVisitorTester* sdmmc_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, sdmmc_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(sdmmc_tester->DoPublish().is_ok());

  auto node_count =
      sdmmc_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        sdmmc_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("mmc-") != std::string::npos) {
      auto metadata = sdmmc_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // sdmmc metadata
      std::vector<uint8_t> metadata_blob = std::move(*(*metadata)[0].data());
      fit::result sdmmc_metadata =
          fidl::Unpersist<fuchsia_hardware_sdmmc::SdmmcMetadata>(metadata_blob);
      ASSERT_TRUE(sdmmc_metadata.is_ok());
      EXPECT_EQ(sdmmc_metadata->instance_identifier(), "mmc@ff000000");
      EXPECT_EQ(sdmmc_metadata->max_frequency(), static_cast<uint32_t>(MAX_FREQUENCY));
      EXPECT_EQ(sdmmc_metadata->removable(), true);
      EXPECT_EQ(sdmmc_metadata->speed_capabilities(),
                fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHs400 |
                    fuchsia_hardware_sdmmc::SdmmcHostPrefs::kDisableHsddr);
      EXPECT_EQ(sdmmc_metadata->use_fidl(), false);

      node_tested_count++;
    }
  }
  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace sdmmc_dt

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../mali-gpu-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <bind/fuchsia/arm/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpu/mali/cpp/bind.h>
#include <gtest/gtest.h>
namespace mali_gpu_dt {

class MaliGpuVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<MaliGpuVisitor> {
 public:
  MaliGpuVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<MaliGpuVisitor>(dtb_path, "MaliGpuVisitorTest") {
  }
};

TEST(MaliGpuVisitorTest, TestBindProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<MaliGpuVisitorTester>("/pkg/test-data/mali-gpu.dtb");
  MaliGpuVisitorTester* mali_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, mali_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(mali_tester->DoPublish().is_ok());

  auto node_count =
      mali_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::non_pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        mali_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::non_pbus_nodes_at, i);

    if (node->args().name()->find("mali-controller") != std::string::npos) {
      node_tested_count++;
      auto mgr_request =
          mali_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, 0);

      ASSERT_EQ(2lu, mgr_request.parents()->size());

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpu_mali::SERVICE,
                                    bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT)}},
          (*mgr_request.parents())[1].bind_rules(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{
              fdf::MakeProperty(bind_fuchsia_hardware_gpu_mali::SERVICE,
                                bind_fuchsia_hardware_gpu_mali::SERVICE_DRIVERTRANSPORT),
          }},
          (*mgr_request.parents())[1].properties(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace mali_gpu_dt

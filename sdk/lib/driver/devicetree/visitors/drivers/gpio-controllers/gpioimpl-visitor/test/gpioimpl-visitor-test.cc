// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../gpioimpl-visitor.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.gpioimpl/cpp/fidl.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <gtest/gtest.h>

#include "dts/gpio.h"
#include "src/lib/ddk/include/ddk/metadata/gpio.h"

namespace gpio_impl_dt {

class GpioImplVisitorTester : public fdf_devicetree::testing::VisitorTestHelper<GpioImplVisitor> {
 public:
  GpioImplVisitorTester(std::string_view dtb_path)
      : fdf_devicetree::testing::VisitorTestHelper<GpioImplVisitor>(dtb_path,
                                                                    "GpioImplVisitorTest") {}
};

TEST(GpioImplVisitorTest, TestGpiosProperty) {
  fdf_devicetree::VisitorRegistry visitors;
  ASSERT_TRUE(
      visitors.RegisterVisitor(std::make_unique<fdf_devicetree::BindPropertyVisitor>()).is_ok());

  auto tester = std::make_unique<GpioImplVisitorTester>("/pkg/test-data/gpio.dtb");
  GpioImplVisitorTester* gpio_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, gpio_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(gpio_tester->DoPublish().is_ok());

  auto node_count =
      gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("gpio-controller") != std::string::npos) {
      auto metadata = gpio_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(2lu, metadata->size());
      std::vector<uint8_t> metadata_blob0 = std::move(*(*metadata)[0].data());
      fit::result init_metadata =
          fidl::Unpersist<fuchsia_hardware_gpioimpl::InitMetadata>(metadata_blob0);
      ASSERT_TRUE(init_metadata.is_ok());
      ASSERT_EQ((*init_metadata).steps().size(), 3u);
      ASSERT_EQ((*init_metadata).steps()[0].index(), static_cast<uint32_t>(HOG_PIN1));
      ASSERT_EQ((*init_metadata).steps()[0].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithOutputValue(0));
      ASSERT_EQ((*init_metadata).steps()[1].index(), static_cast<uint32_t>(HOG_PIN2));
      ASSERT_EQ((*init_metadata).steps()[1].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithInputFlags(
                    static_cast<fuchsia_hardware_gpio::GpioFlags>(HOG_PIN2_FLAG)));
      ASSERT_EQ((*init_metadata).steps()[2].index(), static_cast<uint32_t>(HOG_PIN3));
      ASSERT_EQ((*init_metadata).steps()[2].call(),
                fuchsia_hardware_gpioimpl::InitCall::WithInputFlags(
                    static_cast<fuchsia_hardware_gpio::GpioFlags>(HOG_PIN3_FLAG)));

      std::vector<uint8_t> metadata_blob1 = std::move(*(*metadata)[1].data());
      auto metadata_start = reinterpret_cast<gpio_pin_t*>(metadata_blob1.data());
      std::vector<gpio_pin_t> gpio_pins(
          metadata_start, metadata_start + (metadata_blob1.size() / sizeof(gpio_pin_t)));
      ASSERT_EQ(gpio_pins.size(), 2lu);
      EXPECT_EQ(gpio_pins[0].pin, static_cast<uint32_t>(PIN1));
      EXPECT_EQ(strcmp(gpio_pins[0].name, PIN1_NAME), 0);
      EXPECT_EQ(gpio_pins[1].pin, static_cast<uint32_t>(PIN2));
      EXPECT_EQ(strcmp(gpio_pins[1].name, PIN2_NAME), 0);

      node_tested_count++;
    }
  }

  ASSERT_EQ(node_tested_count, 1u);
}

}  // namespace gpio_impl_dt

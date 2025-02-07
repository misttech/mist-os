// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../gpioimpl-visitor.h"

#include <fidl/fuchsia.hardware.gpio/cpp/fidl.h>
#include <fidl/fuchsia.hardware.pinimpl/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/testing/visitor-test-helper.h>
#include <lib/driver/devicetree/visitors/default/bind-property/bind-property.h>
#include <lib/driver/devicetree/visitors/default/mmio/mmio.h>
#include <lib/driver/devicetree/visitors/registry.h>

#include <cstdint>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <gtest/gtest.h>

#include "dts/gpio.h"

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
  ASSERT_TRUE(visitors.RegisterVisitor(std::make_unique<fdf_devicetree::MmioVisitor>()).is_ok());

  auto tester = std::make_unique<GpioImplVisitorTester>("/pkg/test-data/gpio.dtb");
  GpioImplVisitorTester* gpio_tester = tester.get();
  ASSERT_TRUE(visitors.RegisterVisitor(std::move(tester)).is_ok());

  ASSERT_EQ(ZX_OK, gpio_tester->manager()->Walk(visitors).status_value());
  ASSERT_TRUE(gpio_tester->DoPublish().is_ok());

  auto node_count =
      gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_node_size);

  uint32_t node_tested_count = 0;
  uint32_t mgr_request_idx = 0;
  uint32_t gpioA_id = 0;
  uint32_t gpioB_id = 0;
  for (size_t i = 0; i < node_count; i++) {
    auto node =
        gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);

    if (node.name()->find("gpio-controller-ffffa000") != std::string::npos) {
      auto metadata = gpio_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // Controller metadata.
      std::vector<uint8_t> metadata_blob0 = std::move(*(*metadata)[0].data());
      fit::result controller_metadata =
          fidl::Unpersist<fuchsia_hardware_pinimpl::Metadata>(metadata_blob0);
      ASSERT_TRUE(controller_metadata.is_ok());
      ASSERT_TRUE(controller_metadata->controller_id());
      gpioA_id = *controller_metadata->controller_id();

      ASSERT_TRUE(controller_metadata->init_steps());
      ASSERT_EQ((*controller_metadata).init_steps()->size(),
                6u /*from gpio hog*/ + 8u /*pincfg groups*/);

      // GPIO Hog init steps.
      const auto& init_steps = *controller_metadata->init_steps();
      ASSERT_TRUE(init_steps[0].call());
      ASSERT_EQ(init_steps[0].call()->pin(), static_cast<uint32_t>(HOG_PIN1));
      ASSERT_EQ(init_steps[0].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig(
                    {{.pull = static_cast<fuchsia_hardware_pin::Pull>(0)}}));
      ASSERT_TRUE(init_steps[1].call());
      ASSERT_EQ(init_steps[1].call()->pin(), static_cast<uint32_t>(HOG_PIN1));
      ASSERT_EQ(init_steps[1].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithBufferMode(
                                                  fuchsia_hardware_gpio::BufferMode::kOutputLow));

      ASSERT_TRUE(init_steps[2].call());
      ASSERT_EQ(init_steps[2].call()->pin(), static_cast<uint32_t>(HOG_PIN2));
      ASSERT_EQ(init_steps[2].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig(
                    {{.pull = static_cast<fuchsia_hardware_pin::Pull>(HOG_PIN2_FLAG)}}));
      ASSERT_TRUE(init_steps[3].call());
      ASSERT_EQ(init_steps[3].call()->pin(), static_cast<uint32_t>(HOG_PIN2));
      ASSERT_EQ(init_steps[3].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithBufferMode(
                                                  fuchsia_hardware_gpio::BufferMode::kInput));

      ASSERT_TRUE(init_steps[4].call());
      ASSERT_EQ(init_steps[4].call()->pin(), static_cast<uint32_t>(HOG_PIN3));
      ASSERT_EQ(init_steps[4].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig(
                    {{.pull = static_cast<fuchsia_hardware_pin::Pull>(HOG_PIN3_FLAG)}}));
      ASSERT_TRUE(init_steps[5].call());
      ASSERT_EQ(init_steps[5].call()->pin(), static_cast<uint32_t>(HOG_PIN3));
      ASSERT_EQ(init_steps[5].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithBufferMode(
                                                  fuchsia_hardware_gpio::BufferMode::kInput));

      // Pin controller config init steps.
      ASSERT_TRUE(init_steps[6].call());
      ASSERT_EQ(init_steps[6].call()->pin(), static_cast<uint32_t>(GROUP1_PIN1));
      ASSERT_EQ(init_steps[6].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
                    .function = GROUP1_FUNCTION,
                    .drive_strength_ua = GROUP1_DRIVE_STRENGTH,
                    .drive_type = fuchsia_hardware_pin::DriveType::kOpenDrain,
                }}));

      ASSERT_TRUE(init_steps[7].call());
      ASSERT_EQ(init_steps[7].call()->pin(), static_cast<uint32_t>(GROUP1_PIN2));
      ASSERT_EQ(init_steps[7].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
                    .function = GROUP1_FUNCTION,
                    .drive_strength_ua = GROUP1_DRIVE_STRENGTH,
                    .drive_type = fuchsia_hardware_pin::DriveType::kOpenDrain,
                }}));

      ASSERT_TRUE(init_steps[8].call());
      ASSERT_EQ(init_steps[8].call()->pin(), static_cast<uint32_t>(GROUP3_PIN1));
      ASSERT_EQ(init_steps[8].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
                    .pull = fuchsia_hardware_pin::Pull::kNone,
                    .drive_type = fuchsia_hardware_pin::DriveType::kOpenSource,
                }}));

      ASSERT_TRUE(init_steps[9].call());
      ASSERT_EQ(init_steps[9].call()->pin(), static_cast<uint32_t>(GROUP3_PIN1));
      ASSERT_EQ(init_steps[9].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithBufferMode(
                                                  fuchsia_hardware_gpio::BufferMode::kInput));

      ASSERT_TRUE(init_steps[10].call());
      ASSERT_EQ(init_steps[10].call()->pin(), static_cast<uint32_t>(GROUP2_PIN1));
      ASSERT_EQ(init_steps[10].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithPinConfig(
                                                   {{.power_source = GROUP2_POWER_SOURCE}}));

      ASSERT_TRUE(init_steps[11].call());
      ASSERT_EQ(init_steps[11].call()->pin(), static_cast<uint32_t>(GROUP2_PIN1));
      ASSERT_EQ(init_steps[11].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithBufferMode(
                                                   fuchsia_hardware_gpio::BufferMode::kOutputLow));

      ASSERT_TRUE(init_steps[12].call());
      ASSERT_EQ(init_steps[12].call()->pin(), static_cast<uint32_t>(GROUP2_PIN2));
      ASSERT_EQ(init_steps[12].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithPinConfig(
                                                   {{.power_source = GROUP2_POWER_SOURCE}}));

      ASSERT_TRUE(init_steps[13].call());
      ASSERT_EQ(init_steps[13].call()->pin(), static_cast<uint32_t>(GROUP2_PIN2));
      ASSERT_EQ(init_steps[13].call()->call(), fuchsia_hardware_pinimpl::InitCall::WithBufferMode(
                                                   fuchsia_hardware_gpio::BufferMode::kOutputLow));

      // GPIO Hog init steps.
      ASSERT_TRUE(controller_metadata->pins().has_value());
      ASSERT_EQ((*controller_metadata).pins()->size(), 2lu);
      std::span<fuchsia_hardware_pinimpl::Pin> gpio_pins = controller_metadata->pins().value();
      ASSERT_EQ(gpio_pins.size(), 2lu);
      EXPECT_EQ(gpio_pins[0].pin().value(), static_cast<uint32_t>(PIN1));
      EXPECT_EQ(gpio_pins[0].name().value(), PIN1_NAME);
      EXPECT_EQ(gpio_pins[1].pin().value(), static_cast<uint32_t>(PIN2));
      EXPECT_EQ(gpio_pins[1].name().value(), PIN2_NAME);

      node_tested_count++;
    }

    if (node.name()->find("gpio-controller-ffffb000") != std::string::npos) {
      auto metadata = gpio_tester->env()
                          .SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i)
                          .metadata();

      // Test metadata properties.
      ASSERT_TRUE(metadata);
      ASSERT_EQ(1lu, metadata->size());

      // Controller metadata.
      std::vector<uint8_t> metadata_blob0 = std::move(*(*metadata)[0].data());
      fit::result controller_metadata =
          fidl::Unpersist<fuchsia_hardware_pinimpl::Metadata>(metadata_blob0);
      ASSERT_TRUE(controller_metadata.is_ok());
      ASSERT_TRUE(controller_metadata->controller_id());
      gpioB_id = *controller_metadata->controller_id();

      ASSERT_TRUE(controller_metadata->init_steps());
      ASSERT_EQ((*controller_metadata).init_steps()->size(), 1u);

      // Pin controller config init steps.
      ASSERT_TRUE((*controller_metadata->init_steps())[0].call());
      ASSERT_EQ((*controller_metadata->init_steps())[0].call()->pin(),
                static_cast<uint32_t>(GROUP4_PIN1));
      ASSERT_EQ((*controller_metadata->init_steps())[0].call()->call(),
                fuchsia_hardware_pinimpl::InitCall::WithPinConfig(
                    {{.pull = fuchsia_hardware_pin::Pull::kUp,
                      .drive_type = fuchsia_hardware_pin::DriveType::kPushPull}}));
      node_tested_count++;
    }
  }

  for (size_t i = 0; i < node_count; i++) {
    auto node =
        gpio_tester->env().SyncCall(&fdf_devicetree::testing::FakeEnvWrapper::pbus_nodes_at, i);
    if (node.name()->find("audio") != std::string::npos) {
      node_tested_count++;

      ASSERT_EQ(2lu, gpio_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = gpio_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(4lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd parent is GPIO PIN1.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION,
                              "fuchsia.gpio.FUNCTION." + std::string(PIN1_NAME))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                                    bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, gpioA_id),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(PIN1))}},
          (*mgr_request.parents())[1].bind_rules(), false));

      // 3rd parent is GPIO PIN2.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                              bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION,
                              "fuchsia.gpio.FUNCTION." + std::string(PIN2_NAME))}},
          (*mgr_request.parents())[2].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                                    bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, gpioA_id),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(PIN2))}},
          (*mgr_request.parents())[2].bind_rules(), false));

      // 4th parent is GPIO INIT.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
            fdf::MakeProperty(bind_fuchsia::GPIO_CONTROLLER, static_cast<uint32_t>(0))}},
          (*mgr_request.parents())[3].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                    bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, gpioA_id)}},
          (*mgr_request.parents())[3].bind_rules(), false));
    }

    if (node.name()->find("video") != std::string::npos) {
      node_tested_count++;

      ASSERT_EQ(2lu, gpio_tester->env().SyncCall(
                         &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_size));

      auto mgr_request = gpio_tester->env().SyncCall(
          &fdf_devicetree::testing::FakeEnvWrapper::mgr_requests_at, mgr_request_idx++);
      ASSERT_TRUE(mgr_request.parents().has_value());
      ASSERT_EQ(3lu, mgr_request.parents()->size());

      // 1st parent is pdev. Skipping that.
      // 2nd and 3rd parents are GPIO INIT of different gpio controllers.
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
            fdf::MakeProperty(bind_fuchsia::GPIO_CONTROLLER, static_cast<uint32_t>(0))}},
          (*mgr_request.parents())[1].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                    bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, gpioA_id)}},
          (*mgr_request.parents())[1].bind_rules(), false));

      EXPECT_TRUE(fdf_devicetree::testing::CheckHasProperties(
          {{fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
            fdf::MakeProperty(bind_fuchsia::GPIO_CONTROLLER, static_cast<uint32_t>(1))}},
          (*mgr_request.parents())[2].properties(), false));
      EXPECT_TRUE(fdf_devicetree::testing::CheckHasBindRules(
          {{fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP,
                                    bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
            fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_CONTROLLER, gpioB_id)}},
          (*mgr_request.parents())[2].bind_rules(), false));
    }
  }

  ASSERT_EQ(node_tested_count, 4u);
}

}  // namespace gpio_impl_dt

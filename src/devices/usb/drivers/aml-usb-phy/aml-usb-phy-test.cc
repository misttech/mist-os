// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy.h"

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <soc/aml-common/aml-registers.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/registers/testing/mock-registers/mock-registers.h"
#include "src/devices/usb/drivers/aml-usb-phy/usb-phy-regs.h"

namespace aml_usb_phy {

constexpr auto kRegisterBanks = 4;
constexpr auto kRegisterCount = 2048;

class FakeMmio {
 public:
  FakeMmio() : region_(sizeof(uint32_t), kRegisterCount) {
    for (size_t c = 0; c < kRegisterCount; c++) {
      region_[c * sizeof(uint32_t)].SetReadCallback([this, c]() { return reg_values_[c]; });
      region_[c * sizeof(uint32_t)].SetWriteCallback(
          [this, c](uint64_t value) { reg_values_[c] = value; });
    }
  }

  fdf::MmioBuffer mmio() { return region_.GetMmioBuffer(); }

 private:
  ddk_fake::FakeMmioRegRegion region_;
  uint64_t reg_values_[kRegisterCount] = {};
};

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};

  compat::DeviceServer device_server_;
  fake_pdev::FakePDevFidl pdev_server;
  mock_registers::MockRegisters registers{fdf::Dispatcher::GetCurrent()->async_dispatcher()};
};

// Fixture that supports tests of AmlUsbPhy::Create.
class AmlUsbPhyTest : public zxtest::Test {
 public:
  void SetUp() override {
    static constexpr uint32_t kMagicNumbers[8] = {};

    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    irq_ = config.irqs[0].borrow();
    config.mmios[0] = mmio_[0].mmio();
    config.mmios[1] = mmio_[1].mmio();
    config.mmios[2] = mmio_[2].mmio();
    config.mmios[3] = mmio_[3].mmio();

    fuchsia_driver_framework::DriverStartArgs start_args;
    fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory_client;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_directory_client = std::move(start_args_result->outgoing_directory_client);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->device_server_.Init("pdev", "");

      // Serve metadata.
      auto status = incoming->device_server_.AddMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers,
                                                         sizeof(kMagicNumbers));
      EXPECT_OK(status);
      status = incoming->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env_.incoming_directory());
      EXPECT_OK(status);

      // Serve pdev_server.
      incoming->pdev_server.SetConfig(std::move(config));
      auto result =
          incoming->env_.incoming_directory().AddService<fuchsia_hardware_platform_device::Service>(
              std::move(incoming->pdev_server.GetInstanceHandler(
                  fdf::Dispatcher::GetCurrent()->async_dispatcher())),
              "pdev");
      ASSERT_TRUE(result.is_ok());

      // Serve registers.
      result = incoming->env_.incoming_directory().AddService<fuchsia_hardware_registers::Service>(
          std::move(incoming->registers.GetInstanceHandler()), "register-reset");
      ASSERT_TRUE(result.is_ok());

      // Prepare for Start().
      incoming->registers.ExpectWrite<uint32_t>(RESET1_LEVEL_OFFSET,
                                                aml_registers::USB_RESET1_LEVEL_MASK,
                                                aml_registers::USB_RESET1_LEVEL_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET1_LEVEL_OFFSET,
                                                aml_registers::USB_RESET1_LEVEL_MASK,
                                                ~aml_registers::USB_RESET1_LEVEL_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET1_LEVEL_OFFSET,
                                                aml_registers::USB_RESET1_LEVEL_MASK,
                                                aml_registers::USB_RESET1_LEVEL_MASK);
    });
    ASSERT_NO_FATAL_FAILURE();

    // Start dut_.
    auto result = runtime_.RunToCompletion(dut_.Start(std::move(start_args)));
    ASSERT_TRUE(result.is_ok());

    runtime_.RunUntilIdle();
  }

  void TearDown() override {
    incoming_.SyncCall([](IncomingNamespace* incoming) { incoming->registers.VerifyAll(); });
    ASSERT_NO_FATAL_FAILURE();
  }

  // This method fires the irq and then waits for the side effects of SetMode to have taken place.
  void TriggerInterruptAndCheckMode(AmlUsbPhy::UsbMode mode) {
    auto& phy = dut_->device_;
    // Switch to appropriate mode. This will be read by the irq thread.
    USB_R5_V2::Get()
        .FromValue(0)
        .set_iddig_curr(mode == AmlUsbPhy::UsbMode::PERIPHERAL)
        .WriteTo(&phy->usbctrl_mmio_);
    // Wake up the irq thread.
    ASSERT_OK(irq_->trigger(0, zx::clock::get_monotonic()));
    runtime_.RunUntilIdle();

    // Check that mode is as expected.
    EXPECT_EQ(phy->usbphy2_[0].mode(), AmlUsbPhy::UsbMode::HOST);
    EXPECT_EQ(phy->usbphy2_[1].mode(), mode);
  }

 protected:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};

 private:
  fdf_testing::DriverUnderTest<AmlUsbPhyDevice> dut_;
  FakeMmio mmio_[kRegisterBanks];
  zx::unowned_interrupt irq_;
};

TEST_F(AmlUsbPhyTest, SetMode) {
  fdf_testing::TestNode* phy;
  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    // The aml_usb_phy device should be added.
    ASSERT_EQ(incoming->node_.children().size(), 1);
    ASSERT_NE(incoming->node_.children().find("aml_usb_phy"), incoming->node_.children().end());
    phy = &incoming->node_.children().at("aml_usb_phy");
    // The xhci device child should be added.
    ASSERT_EQ(phy->children().size(), 1);
    EXPECT_NE(phy->children().find("xhci"), phy->children().end());
  });

  // Trigger interrupt configuring initial Host mode.
  TriggerInterruptAndCheckMode(AmlUsbPhy::UsbMode::HOST);
  // Nothing should've changed.
  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    ASSERT_EQ(phy->children().size(), 1);
    // The xhci device child should exist.
    EXPECT_NE(phy->children().find("xhci"), phy->children().end());
  });

  // Trigger interrupt, and switch to Peripheral mode.
  TriggerInterruptAndCheckMode(AmlUsbPhy::UsbMode::PERIPHERAL);
  // The dwc2 device should be added.
  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    ASSERT_EQ(phy->children().size(), 2);
    // The xhci device child should exist.
    EXPECT_NE(phy->children().find("xhci"), phy->children().end());
    // The dwc2 device child should exist.
    EXPECT_NE(phy->children().find("dwc2"), phy->children().end());
  });

  // Trigger interrupt, and switch (back) to Host mode.
  TriggerInterruptAndCheckMode(AmlUsbPhy::UsbMode::HOST);
  // The dwc2 device should be removed.
  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    ASSERT_EQ(phy->children().size(), 1);
    // The xhci device child should exist.
    EXPECT_NE(phy->children().find("xhci"), phy->children().end());
  });
}

}  // namespace aml_usb_phy

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy/aml-usb-phy.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <gtest/gtest.h>
#include <soc/aml-common/aml-registers.h>

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

  uint64_t reg_values_[kRegisterCount] = {0};

 private:
  ddk_fake::FakeMmioRegRegion region_;
};

class FakePDev : public fidl::testing::WireTestBase<fuchsia_hardware_platform_device::Device> {
 public:
  FakePDev() {
    EXPECT_EQ(ZX_OK, zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &interrupt_));
  }

  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler(
      async_dispatcher_t* dispatcher) {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
    });
  }

  zx::interrupt& irq() { return interrupt_; }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}

  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetInterruptById(
      fuchsia_hardware_platform_device::wire::DeviceGetInterruptByIdRequest* request,
      GetInterruptByIdCompleter::Sync& completer) override {
    if (request->index != 0) {
      return completer.ReplyError(ZX_ERR_NOT_FOUND);
    }

    zx::interrupt out_interrupt;
    zx_status_t status = interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &out_interrupt);
    if (status == ZX_OK) {
      completer.ReplySuccess(std::move(out_interrupt));
    } else {
      completer.ReplyError(status);
    }
  }

  zx::interrupt interrupt_;
  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> binding_group_;
};

class TestAmlUsbPhyDevice : public AmlUsbPhyDevice {
 public:
  TestAmlUsbPhyDevice(fdf::DriverStartArgs start_args,
                      fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : AmlUsbPhyDevice(std::move(start_args), std::move(driver_dispatcher)) {}

  static DriverRegistration GetDriverRegistration() {
    return FUCHSIA_DRIVER_REGISTRATION_V1(
        fdf_internal::DriverServer<TestAmlUsbPhyDevice>::initialize,
        fdf_internal::DriverServer<TestAmlUsbPhyDevice>::destroy);
  }

  bool dwc2_connected() { return device()->dwc2_connected(); }
  FakeMmio& usbctrl_mmio() { return mmio_[0]; }

 private:
  zx::result<fdf::MmioBuffer> MapMmio(
      const fidl::WireSyncClient<fuchsia_hardware_platform_device::Device>& pdev,
      uint32_t idx) override {
    if (idx >= kRegisterBanks) {
      return zx::error(ZX_ERR_OUT_OF_RANGE);
    }
    return zx::ok(mmio_[idx].mmio());
  }

  FakeMmio mmio_[kRegisterBanks];
};

struct IncomingNamespace {
  fdf_testing::TestNode node_{std::string("root")};
  fdf_testing::internal::TestEnvironment env_{fdf::Dispatcher::GetCurrent()->get()};

  compat::DeviceServer device_server_;
  FakePDev pdev_server;
  mock_registers::MockRegisters registers{fdf::Dispatcher::GetCurrent()->async_dispatcher()};
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
// Fixture that supports tests of AmlUsbPhy::Create.
class AmlUsbPhyTest : public testing::Test {
 public:
  void SetUp() override {
    static constexpr uint32_t kMagicNumbers[8] = {};
    static constexpr uint8_t kPhyType = kG12A;
    static const std::vector<UsbPhyMode> kPhyModes = {
        {UsbProtocol::Usb2_0, UsbMode::Host, false},
        {UsbProtocol::Usb2_0, UsbMode::Otg, true},
        {UsbProtocol::Usb3_0, UsbMode::Host, false},
    };

    fuchsia_driver_framework::DriverStartArgs start_args;
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      auto start_args_result = incoming->node_.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args_result.is_ok());
      start_args = std::move(start_args_result->start_args);
      outgoing_ = std::move(start_args_result->outgoing_directory_client);

      auto init_result =
          incoming->env_.Initialize(std::move(start_args_result->incoming_directory_server));
      ASSERT_TRUE(init_result.is_ok());

      incoming->device_server_.Initialize("pdev");

      // Serve metadata.
      auto status = incoming->device_server_.AddMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers,
                                                         sizeof(kMagicNumbers));
      EXPECT_EQ(ZX_OK, status);
      status = incoming->device_server_.AddMetadata(
          DEVICE_METADATA_PRIVATE_PHY_TYPE | DEVICE_METADATA_PRIVATE, &kPhyType, sizeof(kPhyType));
      EXPECT_EQ(ZX_OK, status);
      status = incoming->device_server_.AddMetadata(DEVICE_METADATA_USB_MODE, kPhyModes.data(),
                                                    kPhyModes.size() * sizeof(kPhyModes[0]));
      EXPECT_EQ(ZX_OK, status);
      status = incoming->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                              &incoming->env_.incoming_directory());
      EXPECT_EQ(ZX_OK, status);

      // Serve pdev_server.
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
  void TriggerInterruptAndCheckMode(UsbMode mode) {
    // Switch to appropriate mode. This will be read by the irq thread.
    dut_->usbctrl_mmio().reg_values_[USB_R5_OFFSET >> 2] = (mode == UsbMode::Peripheral) << 6;
    // Wake up the irq thread.
    incoming_.SyncCall([](IncomingNamespace* incoming) {
      incoming->pdev_server.irq().trigger(0, zx::clock::get_boot());
    });
    runtime_.RunUntilIdle();

    // Check that mode is as expected.
    auto& phy = dut_->device();
    EXPECT_EQ(phy->usbphy(UsbProtocol::Usb2_0, 0)->phy_mode(), UsbMode::Host);
    EXPECT_EQ(phy->usbphy(UsbProtocol::Usb2_0, 1)->phy_mode(), mode);
    EXPECT_EQ(phy->usbphy(UsbProtocol::Usb3_0, 0)->phy_mode(), UsbMode::Host);
  }

  void CheckDevices(fdf_testing::TestNode* test_node, std::vector<std::string> devices) {
    // Wait for devices
    runtime_.RunUntil(
        [this, &test_node, count = devices.size()]() {
          return incoming_.SyncCall([&test_node, &count](IncomingNamespace* incoming) {
            return test_node->children().size() == count;
          });
        },
        zx::usec(1000));
    // Check devices
    incoming_.SyncCall([&test_node, &devices](IncomingNamespace* incoming) {
      for (auto& dev : devices) {
        EXPECT_NE(test_node->children().find(dev), test_node->children().end());
      }
    });
  }

 protected:
  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{
      env_dispatcher_->async_dispatcher(), std::in_place};
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_;
  fdf_testing::internal::DriverUnderTest<TestAmlUsbPhyDevice> dut_{
      TestAmlUsbPhyDevice::GetDriverRegistration()};
};

TEST_F(AmlUsbPhyTest, SetMode) {
  fdf_testing::TestNode* phy;
  runtime_.RunUntil(
      [this]() {
        return incoming_.SyncCall(
            [&](IncomingNamespace* incoming) { return incoming->node_.children().size() == 1; });
      },
      zx::usec(1000));
  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    // The aml_usb_phy device should be added.
    ASSERT_EQ(incoming->node_.children().size(), 1);
    ASSERT_NE(incoming->node_.children().find("aml_usb_phy"), incoming->node_.children().end());
    phy = &incoming->node_.children().at("aml_usb_phy");
  });
  CheckDevices(phy, {"xhci"});

  // Trigger interrupt configuring initial Host mode.
  TriggerInterruptAndCheckMode(UsbMode::Host);
  // Nothing should've changed.
  CheckDevices(phy, {"xhci"});

  // Trigger interrupt, and switch to Peripheral mode.
  TriggerInterruptAndCheckMode(UsbMode::Peripheral);
  CheckDevices(phy, {"xhci", "dwc2"});

  // Trigger interrupt, and switch (back) to Host mode.
  TriggerInterruptAndCheckMode(UsbMode::Host);
  // The dwc2 device should be removed.
  CheckDevices(phy, {"xhci"});
}

TEST_F(AmlUsbPhyTest, ConnectStatusChanged) {
  fdf_testing::TestNode* phy;
  runtime_.RunUntil(
      [this]() {
        return incoming_.SyncCall(
            [&](IncomingNamespace* incoming) { return incoming->node_.children().size() == 1; });
      },
      zx::usec(1000));
  incoming_.SyncCall([&](IncomingNamespace* incoming) {
    // The aml_usb_phy device should be added.
    ASSERT_EQ(incoming->node_.children().size(), 1);
    ASSERT_NE(incoming->node_.children().find("aml_usb_phy"), incoming->node_.children().end());
    phy = &incoming->node_.children().at("aml_usb_phy");
  });
  CheckDevices(phy, {"xhci"});

  auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
  auto status = fdio_open_at(outgoing_.handle()->get(), "/svc",
                             static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                             endpoints.server.TakeChannel().release());
  EXPECT_EQ(ZX_OK, status);

  auto result = fdf::internal::DriverTransportConnect<fuchsia_hardware_usb_phy::Service::Device>(
      endpoints.client, "xhci");
  ASSERT_TRUE(result.is_ok());

  runtime_.PerformBlockingWork([&result]() {
    fdf::Arena arena('TEST');
    fdf::WireUnownedResult wire_result =
        fdf::WireCall(*result).buffer(arena)->ConnectStatusChanged(true);
    EXPECT_EQ(ZX_OK, wire_result.status());
    ASSERT_TRUE(wire_result.value().is_ok());
  });

  EXPECT_TRUE(dut_->dwc2_connected());
}

}  // namespace aml_usb_phy

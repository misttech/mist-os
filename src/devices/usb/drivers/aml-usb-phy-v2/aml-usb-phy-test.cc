// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy-v2/aml-usb-phy.h"

#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/zx/clock.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>

#include <list>
#include <memory>
#include <queue>
#include <thread>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/registers/testing/mock-registers/mock-registers.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/aml-usb-phy-v2/usb-phy-regs.h"

namespace aml_usb_phy {

enum class RegisterIndex : size_t {
  Control = 0,
  Phy0 = 1,
  Phy1 = 2,
};

constexpr auto kRegisterBanks = 3;
constexpr auto kRegisterCount = 2048;

class FakeMmio {
 public:
  FakeMmio() : region_(sizeof(uint32_t), kRegisterCount) {
    for (size_t c = 0; c < kRegisterCount; c++) {
      region_[c * sizeof(uint32_t)].SetReadCallback([this, c]() { return reg_values_[c]; });
      region_[c * sizeof(uint32_t)].SetWriteCallback([this, c](uint64_t value) {
        reg_values_[c] = value;
        if (callback_) {
          (*callback_)(c, value);
        }
      });
    }
  }

  fdf::MmioBuffer mmio() { return region_.GetMmioBuffer(); }

 private:
  ddk_fake::FakeMmioRegRegion region_;
  std::optional<fit::function<void(size_t reg, uint64_t value)>> callback_;
  uint64_t reg_values_[kRegisterCount] = {};
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  mock_registers::MockRegisters registers{async_get_default_dispatcher()};
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

// Fixture that supports tests of AmlUsbPhy::Create.
class AmlUsbPhyTest : public zxtest::Test {
 public:
  AmlUsbPhyTest() {
    static constexpr uint32_t kMagicNumbers[8] = {};
    root_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers, sizeof(kMagicNumbers));

    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    irq_ = config.irqs[0].borrow();
    config.mmios[0] = mmio_[0].mmio();
    config.mmios[1] = mmio_[1].mmio();
    config.mmios[2] = mmio_[2].mmio();
    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    zx::result registers_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(registers_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server),
                        registers_server = std::move(registers_endpoints->server)](
                           IncomingNamespace* incoming) mutable {
      incoming->pdev_server.SetConfig(std::move(config));
      ASSERT_OK(incoming->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
          incoming->pdev_server.GetInstanceHandler()));
      ASSERT_OK(incoming->outgoing.Serve(std::move(server)));

      ASSERT_OK(incoming->outgoing.AddService<fuchsia_hardware_registers::Service>(
          incoming->registers.GetInstanceHandler()));
      ASSERT_OK(incoming->outgoing.Serve(std::move(registers_server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    root_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                          std::move(outgoing_endpoints->client), "pdev");
    root_->AddFidlService(fuchsia_hardware_registers::Service::Name,
                          std::move(registers_endpoints->client), "register-reset");

    incoming_.SyncCall([](IncomingNamespace* incoming) {
      incoming->registers.ExpectWrite<uint32_t>(RESET1_LEVEL_OFFSET,
                                                aml_registers::USB_RESET1_LEVEL_MASK,
                                                aml_registers::USB_RESET1_LEVEL_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK,
                                                aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK);
    });
    ASSERT_NO_FATAL_FAILURE();

    ASSERT_OK(AmlUsbPhy::Create(nullptr, root_.get()));
  }

  void TearDown() override {
    incoming_.SyncCall([](IncomingNamespace* incoming) { incoming->registers.VerifyAll(); });
    ASSERT_NO_FATAL_FAILURE();
  }

  void TriggerInterrupt(ddk::PDevFidl& pdev, AmlUsbPhy::UsbMode mode) {
    std::optional<fdf::MmioBuffer> usbctrl_mmio;
    ASSERT_OK(pdev.MapMmio(0, &usbctrl_mmio));

    // Switch to appropriate mode. This will be read by the irq thread.
    USB_R5_V2::Get()
        .FromValue(0)
        .set_iddig_curr(mode == AmlUsbPhy::UsbMode::PERIPHERAL)
        .WriteTo(&usbctrl_mmio.value());
    // Wake up the irq thread.
    ASSERT_OK(irq_->trigger(0, zx::clock::get_monotonic()));
  }

 protected:
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();

 private:
  FakeMmio mmio_[kRegisterBanks];
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  zx::unowned_interrupt irq_;
};

TEST_F(AmlUsbPhyTest, SetMode) {
  // The aml-usb-phy device should be added.
  auto* mock_phy = root_->GetLatestChild();
  ASSERT_NOT_NULL(mock_phy);
  auto* phy = mock_phy->GetDeviceContext<AmlUsbPhy>();

  // Call DdkInit
  mock_phy->InitOp();
  mock_phy->WaitUntilInitReplyCalled();
  EXPECT_TRUE(mock_phy->InitReplyCalled());
  auto* mock_xhci = mock_phy->GetLatestChild();
  ASSERT_NOT_NULL(mock_xhci);
  auto* xhci = mock_xhci->GetDeviceContext<void>();
  ASSERT_EQ(xhci, phy);

  ASSERT_EQ(phy->mode(), AmlUsbPhy::UsbMode::HOST);

  // Trigger interrupt, and switch to Peripheral mode.
  TriggerInterrupt(phy->pdev(), AmlUsbPhy::UsbMode::PERIPHERAL);
  mock_xhci->WaitUntilAsyncRemoveCalled();
  EXPECT_TRUE(mock_xhci->AsyncRemoveCalled());
  mock_ddk::ReleaseFlaggedDevices(root_.get());
  ASSERT_EQ(mock_phy->child_count(), 1);
  auto* mock_dwc2 = mock_phy->GetLatestChild();
  ASSERT_NOT_NULL(mock_dwc2);
  auto* dwc2 = mock_dwc2->GetDeviceContext<void>();
  ASSERT_EQ(dwc2, phy);

  ASSERT_EQ(phy->mode(), AmlUsbPhy::UsbMode::PERIPHERAL);

  // Trigger interrupt, and switch (back) to Host mode.
  TriggerInterrupt(phy->pdev(), AmlUsbPhy::UsbMode::HOST);
  mock_dwc2->WaitUntilAsyncRemoveCalled();
  EXPECT_TRUE(mock_dwc2->AsyncRemoveCalled());
  mock_ddk::ReleaseFlaggedDevices(root_.get());
  ASSERT_EQ(mock_phy->child_count(), 1);
  mock_xhci = mock_phy->GetLatestChild();
  ASSERT_NOT_NULL(mock_xhci);
  xhci = mock_xhci->GetDeviceContext<void>();
  ASSERT_EQ(xhci, phy);

  ASSERT_EQ(phy->mode(), AmlUsbPhy::UsbMode::HOST);
  ASSERT_EQ(mock_phy->child_count(), 1);
}

}  // namespace aml_usb_phy

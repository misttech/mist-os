// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-crg-phy-v2/aml-usb-crg-phy.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/default.h>
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
#include "src/devices/testing/fake-mmio-reg/include/fake-mmio-reg/fake-mmio-reg.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/aml-usb-crg-phy-v2/usb-phy-regs.h"

namespace aml_usb_crg_phy {

constexpr auto kRegisterBanks = 3;
constexpr auto kRegisterCount = 2048;
enum class RegisterIndex : size_t {
  Control = 0,
  Phy0 = 1,
  Phy1 = 2,
};

class FakeMmio {
 public:
  FakeMmio() : mmio_(sizeof(uint32_t), kRegisterCount) {
    for (size_t c = 0; c < kRegisterCount; c++) {
      mmio_[c].SetReadCallback([this, c]() { return reg_values_[c]; });
      mmio_[c].SetWriteCallback([this, c](uint64_t value) { reg_values_[c] = value; });
    }
  }

  fdf::MmioBuffer mmio() { return mmio_.GetMmioBuffer(); }

  ddk_fake::FakeMmioReg& reg(size_t ix) { return mmio_[ix]; }

 private:
  uint64_t reg_values_[kRegisterCount] = {};
  ddk_fake::FakeMmioRegRegion mmio_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  mock_registers::MockRegisters registers{async_get_default_dispatcher()};
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

// Fixture that supports tests of AmlUsbCrgPhy::Create.
class AmlUsbCrgPhyTest : public zxtest::Test {
 public:
  void SetUp() override {
    root_device_ = MockDevice::FakeRootParent();

    static constexpr uint32_t kMagicNumbers[8] = {};
    root_device_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers, sizeof(kMagicNumbers));

    loop_.StartThread("incoming-ns-thread");

    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    irq_signaller_ = config.irqs[0].borrow();
    config.mmios[0] = mmio_[0].mmio();
    fprintf(stderr, "mmio 0 virtual address: %p\n", mmio_[0].mmio().get());
    config.mmios[1] = mmio_[1].mmio();
    config.mmios[2] = mmio_[2].mmio();
    config.device_info = {
        .mmio_count = 3,
        .irq_count = 1,
    };

    zx::result registers_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(registers_endpoints);
    zx::result pdev_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(pdev_endpoints);
    incoming_.SyncCall(
        [config = std::move(config), registers_server = std::move(registers_endpoints->server),
         pdev_server_end = std::move(pdev_endpoints->server)](IncomingNamespace* incoming) mutable {
          ASSERT_OK(incoming->outgoing.AddService<fuchsia_hardware_registers::Service>(
              incoming->registers.GetInstanceHandler()));
          ASSERT_OK(incoming->outgoing.Serve(std::move(registers_server)));
          ASSERT_OK(incoming->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
              incoming->pdev_server.GetInstanceHandler()));
          ASSERT_OK(incoming->outgoing.Serve(std::move(pdev_server_end)));
          incoming->pdev_server.SetConfig(std::move(config));
        });
    ASSERT_NO_FATAL_FAILURE();
    root_device_->AddFidlService(fuchsia_hardware_registers::Service::Name,
                                 std::move(registers_endpoints->client), "register-reset");
    root_device_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                                 std::move(pdev_endpoints->client), "pdev");

    incoming_.SyncCall([](IncomingNamespace* incoming) {
      incoming->registers.ExpectWrite<uint32_t>(RESET0_LEVEL_OFFSET,
                                                aml_registers::A5_USB_RESET0_LEVEL_MASK,
                                                aml_registers::A5_USB_RESET0_LEVEL_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET0_REGISTER_OFFSET,
                                                aml_registers::A5_USB_RESET0_MASK,
                                                aml_registers::A5_USB_RESET0_MASK);
      incoming->registers.ExpectWrite<uint32_t>(RESET0_LEVEL_OFFSET,
                                                aml_registers::A5_USB_RESET0_LEVEL_MASK,
                                                aml_registers::A5_USB_RESET0_LEVEL_MASK);
    });
    ASSERT_NO_FATAL_FAILURE();

    ASSERT_OK(AmlUsbCrgPhy::Create(nullptr, parent()));
    ASSERT_EQ(1, parent()->child_count());
    mock_dev_ = parent()->GetLatestChild();
    dut_ = mock_dev_->GetDeviceContext<AmlUsbCrgPhy>();
    mock_dev_->InitOp();
    mock_dev_->WaitUntilInitReplyCalled();
    ASSERT_EQ(1, mock_dev_->child_count());
  }

  void TearDown() override {
    incoming_.SyncCall([](IncomingNamespace* incoming) { incoming->registers.VerifyAll(); });
    ASSERT_NO_FATAL_FAILURE();
  }

  void Interrupt() { irq_signaller_->trigger(0, zx::clock::get_monotonic()); }

  zx_device_t* parent() { return root_device_.get(); }

 protected:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::shared_ptr<MockDevice> root_device_;
  AmlUsbCrgPhy* dut_;
  MockDevice* mock_dev_;
  zx::unowned_interrupt irq_signaller_;
  FakeMmio mmio_[kRegisterBanks];
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{loop_.dispatcher(),
                                                                   std::in_place};
};

TEST_F(AmlUsbCrgPhyTest, SetMode) {
  fdf::MmioBuffer usbctrl_mmio = mmio_[0].mmio();

  // The aml-usb-phy device was added in SetUp().
  ASSERT_EQ(dut_->mode(), AmlUsbCrgPhy::UsbMode::HOST);
  ASSERT_EQ(1, mock_dev_->child_count());
  auto* xhci_dev_ = mock_dev_->GetLatestChild();

  // Switch to peripheral mode. This will be read by the irq thread.
  USB_R5_V2::Get().FromValue(0).set_iddig_curr(1).WriteTo(&usbctrl_mmio);
  // Wake up the irq thread.
  Interrupt();
  xhci_dev_->WaitUntilAsyncRemoveCalled();
  mock_ddk::ReleaseFlaggedDevices(mock_dev_);

  ASSERT_EQ(dut_->mode(), AmlUsbCrgPhy::UsbMode::PERIPHERAL);
  ASSERT_EQ(1, mock_dev_->child_count());
  auto* udc_dev_ = mock_dev_->GetLatestChild();

  // Switch back to host mode. This will be read by the irq thread.
  USB_R5_V2::Get().FromValue(0).set_iddig_curr(0).WriteTo(&usbctrl_mmio);
  // Wake up the irq thread.
  Interrupt();
  udc_dev_->WaitUntilAsyncRemoveCalled();
  mock_ddk::ReleaseFlaggedDevices(mock_dev_);

  ASSERT_EQ(dut_->mode(), AmlUsbCrgPhy::UsbMode::HOST);
  ASSERT_EQ(1, mock_dev_->child_count());
}

}  // namespace aml_usb_crg_phy

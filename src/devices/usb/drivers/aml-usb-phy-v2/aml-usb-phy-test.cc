// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-phy-v2/aml-usb-phy.h"

#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
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
#include "src/devices/usb/drivers/aml-usb-phy-v2/aml_usb_phy_bind.h"
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
  FakeMmio() {
    for (size_t c = 0; c < kRegisterCount; c++) {
      regs_[c].SetReadCallback([this, c]() { return reg_values_[c]; });
      regs_[c].SetWriteCallback([this, c](uint64_t value) {
        reg_values_[c] = value;
        if (callback_) {
          (*callback_)(c, value);
        }
      });
    }
    region_.emplace(regs_, sizeof(uint32_t), kRegisterCount);
  }

  fake_pdev::MmioInfo mmio_info() { return {.offset = reinterpret_cast<size_t>(this)}; }

  fdf::MmioBuffer mmio() { return region_->GetMmioBuffer(); }

 private:
  ddk_fake::FakeMmioReg regs_[kRegisterCount];
  std::optional<ddk_fake::FakeMmioRegRegion> region_;

  std::optional<fit::function<void(size_t reg, uint64_t value)>> callback_;
  uint64_t reg_values_[kRegisterCount] = {};
};

// Fixture that supports tests of AmlUsbPhy::Create.
class AmlUsbPhyTest : public zxtest::Test {
 public:
  AmlUsbPhyTest() {
    static constexpr uint32_t kMagicNumbers[8] = {};
    root_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers, sizeof(kMagicNumbers));

    loop_.StartThread("aml-usb-phy-test-thread");
    registers_device_ = std::make_unique<mock_registers::MockRegistersDevice>(loop_.dispatcher());

    irq_ = pdev_.CreateVirtualInterrupt(0);
    pdev_.set_mmio(0, mmio_[0].mmio_info());
    pdev_.set_mmio(1, mmio_[1].mmio_info());
    pdev_.set_mmio(2, mmio_[2].mmio_info());
    root_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx, "pdev");
    root_->AddProtocol(ZX_PROTOCOL_REGISTERS, registers_device_->proto()->ops,
                       registers_device_->proto()->ctx, "register-reset");

    registers()->ExpectWrite<uint32_t>(RESET1_LEVEL_OFFSET, aml_registers::USB_RESET1_LEVEL_MASK,
                                       aml_registers::USB_RESET1_LEVEL_MASK);
    registers()->ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                       aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK,
                                       aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK);
    registers()->ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                       aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK,
                                       aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK);
    registers()->ExpectWrite<uint32_t>(RESET1_REGISTER_OFFSET,
                                       aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK,
                                       aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK);
    ASSERT_OK(AmlUsbPhy::Create(nullptr, root_.get()));
  }

  void TearDown() override {
    EXPECT_OK(registers()->VerifyAll());

    loop_.Shutdown();
  }

  mock_registers::MockRegisters* registers() { return registers_device_->fidl_service(); }
  void TriggerInterrupt(AmlUsbPhy::UsbMode mode) {
    ddk::PDev client(pdev_.proto());
    std::optional<fdf::MmioBuffer> usbctrl_mmio;
    ASSERT_OK(client.MapMmio(0, &usbctrl_mmio));

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
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  FakeMmio mmio_[kRegisterBanks];
  fake_pdev::FakePDev pdev_;
  zx::unowned_interrupt irq_;
  std::unique_ptr<mock_registers::MockRegistersDevice> registers_device_;
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
  TriggerInterrupt(AmlUsbPhy::UsbMode::PERIPHERAL);
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
  TriggerInterrupt(AmlUsbPhy::UsbMode::HOST);
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

zx_status_t ddk::PDev::MapMmio(uint32_t index, std::optional<MmioBuffer>* mmio,
                               uint32_t cache_policy) {
  pdev_mmio_t pdev_mmio;
  zx_status_t status = GetMmio(index, &pdev_mmio);
  if (status != ZX_OK) {
    return status;
  }
  auto* src = reinterpret_cast<aml_usb_phy::FakeMmio*>(pdev_mmio.offset);
  mmio->emplace(src->mmio());
  return ZX_OK;
}

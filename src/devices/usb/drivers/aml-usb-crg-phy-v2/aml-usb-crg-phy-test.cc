// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/aml-usb-crg-phy-v2/aml-usb-crg-phy.h"

#include <fuchsia/hardware/platform/device/c/banjo.h>
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

#include "src/devices/registers/testing/mock-registers/mock-registers.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "src/devices/usb/drivers/aml-usb-crg-phy-v2/usb-phy-regs.h"

namespace aml_usb_crg_phy {

enum class RegisterIndex : size_t {
  Control = 0,
  Phy0 = 1,
  Phy1 = 2,
};

constexpr auto kRegisterBanks = 3;
constexpr auto kRegisterCount = 2048;

class FakePDev : public ddk::PDevProtocol<FakePDev, ddk::base_protocol> {
 public:
  FakePDev() : pdev_({&pdev_protocol_ops_, this}) {
    // Initialize register read/write hooks.
    for (size_t i = 0; i < kRegisterBanks; i++) {
      regions_[i].emplace(sizeof(uint32_t), kRegisterCount);

      for (size_t c = 0; c < kRegisterCount; c++) {
        (*regions_[i])[c * sizeof(uint32_t)].SetReadCallback(
            [this, i, c]() { return reg_values_[i][c]; });

        (*regions_[i])[c * sizeof(uint32_t)].SetWriteCallback([this, i, c](uint64_t value) {
          reg_values_[i][c] = value;
          if (callback_) {
            (*callback_)(i, c, value);
          }
        });
      }
    }
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq_));
  }

  void SetWriteCallback(fit::function<void(size_t bank, size_t reg, uint64_t value)> callback) {
    callback_ = std::move(callback);
  }

  const pdev_protocol_t* proto() const { return &pdev_; }

  zx_status_t PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio) {
    out_mmio->offset = reinterpret_cast<size_t>(&regions_[index]);
    return ZX_OK;
  }

  fdf::MmioBuffer mmio(RegisterIndex index) {
    return fdf::MmioBuffer(regions_[static_cast<size_t>(index)]->GetMmioBuffer());
  }

  zx_status_t PDevGetInterrupt(uint32_t index, uint32_t flags, zx::interrupt* out_irq) {
    irq_signaller_ = zx::unowned_interrupt(irq_);
    *out_irq = std::move(irq_);
    return ZX_OK;
  }

  void Interrupt() { irq_signaller_->trigger(0, zx::clock::get_monotonic()); }

  zx_status_t PDevGetBti(uint32_t index, zx::bti* out_bti) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t PDevGetSmc(uint32_t index, zx::resource* out_resource) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t PDevGetDeviceInfo(pdev_device_info_t* out_info) { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t PDevGetBoardInfo(pdev_board_info_t* out_info) { return ZX_ERR_NOT_SUPPORTED; }

  ~FakePDev() {}

 private:
  std::optional<fit::function<void(size_t bank, size_t reg, uint64_t value)>> callback_;
  zx::unowned_interrupt irq_signaller_;
  zx::interrupt irq_;
  uint64_t reg_values_[kRegisterBanks][kRegisterCount] = {};
  std::optional<ddk_fake::FakeMmioRegRegion> regions_[kRegisterBanks];
  pdev_protocol_t pdev_;
};

struct IncomingNamespace {
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

    zx::result registers_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(registers_endpoints);
    incoming_.SyncCall([registers_server = std::move(registers_endpoints->server)](
                           IncomingNamespace* incoming) mutable {
      ASSERT_OK(incoming->outgoing.AddService<fuchsia_hardware_registers::Service>(
          incoming->registers.GetInstanceHandler()));
      ASSERT_OK(incoming->outgoing.Serve(std::move(registers_server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    root_device_->AddFidlService(fuchsia_hardware_registers::Service::Name,
                                 std::move(registers_endpoints->client), "register-reset");

    root_device_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx, "pdev");

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

  zx_device_t* parent() { return root_device_.get(); }

 protected:
  async::Loop loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  std::shared_ptr<MockDevice> root_device_;
  AmlUsbCrgPhy* dut_;
  MockDevice* mock_dev_;
  FakePDev pdev_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{loop_.dispatcher(),
                                                                   std::in_place};
};

TEST_F(AmlUsbCrgPhyTest, SetMode) {
  ddk::PDev client(pdev_.proto());
  std::optional<fdf::MmioBuffer> usbctrl_mmio;
  ASSERT_OK(client.MapMmio(0, &usbctrl_mmio));

  // The aml-usb-phy device was added in SetUp().
  ASSERT_EQ(dut_->mode(), AmlUsbCrgPhy::UsbMode::HOST);
  ASSERT_EQ(1, mock_dev_->child_count());
  auto* xhci_dev_ = mock_dev_->GetLatestChild();

  // Switch to peripheral mode. This will be read by the irq thread.
  USB_R5_V2::Get().FromValue(0).set_iddig_curr(1).WriteTo(&usbctrl_mmio.value());
  // Wake up the irq thread.
  pdev_.Interrupt();
  xhci_dev_->WaitUntilAsyncRemoveCalled();
  mock_ddk::ReleaseFlaggedDevices(mock_dev_);

  ASSERT_EQ(dut_->mode(), AmlUsbCrgPhy::UsbMode::PERIPHERAL);
  ASSERT_EQ(1, mock_dev_->child_count());
  auto* udc_dev_ = mock_dev_->GetLatestChild();

  // Switch back to host mode. This will be read by the irq thread.
  USB_R5_V2::Get().FromValue(0).set_iddig_curr(0).WriteTo(&usbctrl_mmio.value());
  // Wake up the irq thread.
  pdev_.Interrupt();
  udc_dev_->WaitUntilAsyncRemoveCalled();
  mock_ddk::ReleaseFlaggedDevices(mock_dev_);

  ASSERT_EQ(dut_->mode(), AmlUsbCrgPhy::UsbMode::HOST);
  ASSERT_EQ(1, mock_dev_->child_count());
}

}  // namespace aml_usb_crg_phy

zx_status_t ddk::PDev::MapMmio(uint32_t index, std::optional<MmioBuffer>* mmio,
                               uint32_t cache_policy) {
  pdev_mmio_t pdev_mmio;
  zx_status_t status = GetMmio(index, &pdev_mmio);
  if (status != ZX_OK) {
    return status;
  }
  auto* src = reinterpret_cast<ddk_fake::FakeMmioRegRegion*>(pdev_mmio.offset);
  mmio->emplace(src->GetMmioBuffer());
  return ZX_OK;
}

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/a1-usb-phy/a1-usb-phy.h"

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
#include "src/devices/usb/drivers/a1-usb-phy/a1_usb_phy_bind.h"
#include "src/devices/usb/drivers/a1-usb-phy/usb-phy-regs.h"

namespace a1_usb_phy {

enum class RegisterIndex : size_t {
  Control = 0,
  Phy = 1,
  Reset = 2,
  Clk = 3,
};

constexpr auto kRegisterBanks = 4;
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

// Fixture that supports tests of A1UsbPhy::Create.
class A1UsbPhyTest : public zxtest::Test {
 public:
  A1UsbPhyTest() {
    static constexpr uint32_t kMagicNumbers[8] = {};
    root_->SetMetadata(DEVICE_METADATA_PRIVATE, &kMagicNumbers, sizeof(kMagicNumbers));

    pdev_.set_mmio(0, mmio_[0].mmio_info());
    pdev_.set_mmio(1, mmio_[1].mmio_info());
    pdev_.set_mmio(2, mmio_[2].mmio_info());
    pdev_.set_mmio(3, mmio_[3].mmio_info());
    ASSERT_OK(pdev_.PDevGetSmc(0, &smc_monitor_));

    root_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx, "pdev");

    ASSERT_OK(A1UsbPhy::Create(nullptr, root_.get()));
  }

  void TearDown() override {}

 protected:
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();

 private:
  FakeMmio mmio_[kRegisterBanks];
  fake_pdev::FakePDev pdev_;
  zx::resource smc_monitor_;
};

}  // namespace a1_usb_phy

zx_status_t ddk::PDev::MapMmio(uint32_t index, std::optional<MmioBuffer>* mmio,
                               uint32_t cache_policy) {
  pdev_mmio_t pdev_mmio;
  zx_status_t status = GetMmio(index, &pdev_mmio);
  if (status != ZX_OK) {
    return status;
  }
  auto* src = reinterpret_cast<a1_usb_phy::FakeMmio*>(pdev_mmio.offset);
  mmio->emplace(src->mmio());
  return ZX_OK;
}

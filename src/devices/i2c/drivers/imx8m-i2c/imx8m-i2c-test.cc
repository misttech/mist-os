// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-i2c.h"

#include <fuchsia/hardware/i2cimpl/c/banjo.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>

#include <atomic>
#include <memory>
#include <optional>
#include <vector>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace imx8m_i2c {

constexpr size_t kRegSize = kRegisterSetSize / sizeof(uint16_t);
static constexpr size_t kMaxTransferSize = (UINT16_MAX - 1);

class FakeMmio {
 public:
  FakeMmio() {
    regs_ = std::make_unique<ddk_fake::FakeMmioReg[]>(kRegSize);
    mmio_ = std::make_unique<ddk_fake::FakeMmioRegRegion>(regs_.get(), sizeof(uint16_t), kRegSize);
  }

  fake_pdev::MmioInfo mmio_info() { return {.offset = reinterpret_cast<size_t>(this)}; }

  fdf::MmioBuffer mmio() { return fdf::MmioBuffer(mmio_->GetMmioBuffer()); }

  ddk_fake::FakeMmioReg& reg(size_t ix) { return regs_[ix >> 1]; }

 private:
  std::unique_ptr<ddk_fake::FakeMmioReg[]> regs_;
  std::unique_ptr<ddk_fake::FakeMmioRegRegion> mmio_;
};

class Imx8mI2cTest : public zxtest::Test {
 public:
  Imx8mI2cTest() {}

  void SetUp() override {
    fdf_env_start();
    zx::result result = test_driver_dispatcher_.Start(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "test-driver-dispatcher");
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  void TearDown() override {
    zx::result result = test_driver_dispatcher_.Stop();
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  void CreateDut() {
    irq_signaller_ = pdev_.CreateVirtualInterrupt(0);

    pdev_.set_device_info(pdev_device_info_t{
        .vid = PDEV_VID_NXP,
        .pid = PDEV_PID_IMX8MMEVK,
        .did = PDEV_DID_IMX_I2C,
        .mmio_count = 1,
        .irq_count = 1,
    });

    pdev_.set_mmio(0, mmio_.mmio_info());
    fake_parent_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx, "pdev");

    zx::result result = fdf::RunOnDispatcherSync(
        DriverDispatcher(), [&]() { EXPECT_OK(Imx8mI2c::Create(nullptr, fake_parent_.get())); });
    EXPECT_EQ(ZX_OK, result.status_value());
    ASSERT_EQ(1, fake_parent_->child_count());
    auto* child = fake_parent_->GetLatestChild();
    dut_ = child->GetDeviceContext<Imx8mI2c>();
    dut_->SetTimeout(zx::duration::infinite());
  }

  void Destroy() {
    auto* child = fake_parent_->GetLatestChild();
    zx::result result = fdf::RunOnDispatcherSync(DriverDispatcher(), [&]() { child->UnbindOp(); });
    EXPECT_EQ(ZX_OK, result.status_value());
    EXPECT_TRUE(child->UnbindReplyCalled());
    result = fdf::RunOnDispatcherSync(DriverDispatcher(), [&]() { child->ReleaseOp(); });
    EXPECT_EQ(ZX_OK, result.status_value());
  }

  void InjectInterrupt() { irq_signaller_->trigger(0, zx::clock::get_monotonic()); }
  async_dispatcher_t* DriverDispatcher() { return test_driver_dispatcher_.dispatcher(); }

 protected:
  std::shared_ptr<MockDevice> fake_parent_{MockDevice::FakeRootParent()};
  FakeMmio mmio_;
  Imx8mI2c* dut_;

 private:
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_;
  fake_pdev::FakePDev pdev_;
  zx::unowned_interrupt irq_signaller_;
};

TEST_F(Imx8mI2cTest, DdkLifecycle) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mI2cTest, I2cImplGetBusCount) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  EXPECT_EQ(dut_->I2cImplGetBusCount(), 1);
  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mI2cTest, I2cImplGetMaxTransferSize) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  size_t out_size;
  ASSERT_OK(dut_->I2cImplGetMaxTransferSize(0, &out_size));
  EXPECT_EQ(out_size, kMaxTransferSize);
  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mI2cTest, I2cImplTransact) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());

  uint8_t read_data = 0;
  i2c_impl_op_t impl_op;
  impl_op.address = 0x51;
  impl_op.data_size = 1;
  impl_op.is_read = true;
  impl_op.stop = true;
  impl_op.data_buffer = &read_data;

  static uint16_t ibb = 0;
  mmio_.reg(StatusReg::Get().addr()).SetReadCallback([&]() {
    uint16_t reg_value = StatusReg::Get().FromValue(0).set_ibb(ibb).set_iif(1).reg_value();
    ibb = !ibb;
    return reg_value;
  });
  mmio_.reg(DataReg::Get().addr()).SetReadCallback([this]() {
    InjectInterrupt();
    return 0xbb;
  });
  mmio_.reg(DataReg::Get().addr()).SetWriteCallback([this](uint64_t value) {
    EXPECT_EQ(value, ((0x51 << 1) | 1));
    InjectInterrupt();
  });

  ASSERT_OK(dut_->I2cImplTransact(0, &impl_op, 1));

  EXPECT_EQ(read_data, 0xbb);

  ASSERT_NO_FATAL_FAILURE(Destroy());
}

}  // namespace imx8m_i2c

// Redefine PDevMakeMmioBufferWeak per the recommendation in pdev.h.
zx_status_t ddk::PDevMakeMmioBufferWeak(const pdev_mmio_t& pdev_mmio,
                                        std::optional<MmioBuffer>* mmio, uint32_t cache_policy) {
  auto* test_harness = reinterpret_cast<imx8m_i2c::FakeMmio*>(pdev_mmio.offset);
  mmio->emplace(test_harness->mmio());
  return ZX_OK;
}

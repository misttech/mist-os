// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-i2c.h"

#include <fuchsia/hardware/i2cimpl/c/banjo.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
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

static constexpr size_t kMaxTransferSize = (UINT16_MAX - 1);

class FakeMmio {
 public:
  FakeMmio() : mmio_(sizeof(uint16_t), kRegisterSetSize) {}

  fdf::MmioBuffer mmio() { return mmio_.GetMmioBuffer(); }

  ddk_fake::FakeMmioReg& reg(size_t ix) { return mmio_[ix]; }

 private:
  ddk_fake::FakeMmioRegRegion mmio_;
};

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
  component::OutgoingDirectory outgoing{async_get_default_dispatcher()};
};

class Imx8mI2cTest : public zxtest::Test {
 public:
  void CreateDut() {
    fake_pdev::FakePDevFidl::Config config;
    config.irqs[0] = {};
    ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &config.irqs[0]));
    irq_signaller_ = config.irqs[0].borrow();
    config.mmios[0] = mmio_.mmio();
    config.device_info = {
        .vid = PDEV_VID_NXP,
        .pid = PDEV_PID_IMX8MMEVK,
        .did = PDEV_DID_IMX_I2C,
        .mmio_count = 1,
        .irq_count = 1,
    };

    zx::result outgoing_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_OK(outgoing_endpoints);
    ASSERT_OK(incoming_loop_.StartThread("incoming-ns-thread"));
    incoming_.SyncCall([config = std::move(config), server = std::move(outgoing_endpoints->server)](
                           IncomingNamespace* infra) mutable {
      infra->pdev_server.SetConfig(std::move(config));
      ASSERT_OK(infra->outgoing.AddService<fuchsia_hardware_platform_device::Service>(
          infra->pdev_server.GetInstanceHandler()));
      ASSERT_OK(infra->outgoing.Serve(std::move(server)));
    });
    ASSERT_NO_FATAL_FAILURE();
    fake_parent_->AddFidlService(fuchsia_hardware_platform_device::Service::Name,
                                 std::move(outgoing_endpoints->client), "pdev");

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
  // TODO(fxb/124464): Migrate test to use dispatcher integration.
  std::shared_ptr<MockDevice> fake_parent_{
      MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED()};
  FakeMmio mmio_;
  Imx8mI2c* dut_;

 private:
  fdf_testing::DriverRuntimeEnv managed_env_;
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherManaged};

  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  zx::unowned_interrupt irq_signaller_;
};

TEST_F(Imx8mI2cTest, DdkLifecycle) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  ASSERT_NO_FATAL_FAILURE(Destroy());
}

TEST_F(Imx8mI2cTest, I2cImplGetMaxTransferSize) {
  ASSERT_NO_FATAL_FAILURE(CreateDut());
  size_t out_size;
  ASSERT_OK(dut_->I2cImplGetMaxTransferSize(&out_size));
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

  ASSERT_OK(dut_->I2cImplTransact(&impl_op, 1));

  EXPECT_EQ(read_data, 0xbb);

  ASSERT_NO_FATAL_FAILURE(Destroy());
}

}  // namespace imx8m_i2c

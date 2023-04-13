// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-nna.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/mmio/mmio.h>

#include <mock-mmio-reg/mock-mmio-reg.h>

#include "s905d3-nna-regs.h"
#include "src/devices/registers/testing/mock-registers/mock-registers.h"
#include "src/devices/testing/mock-ddk/mock-device.h"
#include "t931-nna-regs.h"

namespace {
constexpr size_t kHiuRegSize = 0x2000 / sizeof(uint32_t);
constexpr size_t kPowerRegSize = 0x1000 / sizeof(uint32_t);
constexpr size_t kMemoryPDRegSize = 0x1000 / sizeof(uint32_t);
}  // namespace

namespace aml_nna {

class MockRegistersInternal {
 public:
  MockRegistersInternal()
      : hiu_mock_(ddk_mock::MockMmioRegRegion(sizeof(uint32_t), kHiuRegSize)),
        power_mock_(ddk_mock::MockMmioRegRegion(sizeof(uint32_t), kPowerRegSize)),
        memory_pd_mock_(ddk_mock::MockMmioRegRegion(sizeof(uint32_t), kMemoryPDRegSize)) {
    loop_.StartThread();
    reset_mock_ = std::make_unique<mock_registers::MockRegisters>(loop_.dispatcher());
  }

  // The caller should set the mock expectations before calling this.
  void CreateDeviceAndVerify(AmlNnaDevice::NnaBlock nna_block) {
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_registers::Device>();
    ASSERT_OK(endpoints);
    auto& [client_end, server_end] = endpoints.value();
    reset_mock_->Init(std::move(server_end));

    ddk::PDevFidl pdev;
    zx::resource smc_monitor;
    auto device = std::make_unique<AmlNnaDevice>(
        fake_parent_.get(), hiu_mock_.GetMmioBuffer(), power_mock_.GetMmioBuffer(),
        memory_pd_mock_.GetMmioBuffer(), std::move(client_end), std::move(pdev), nna_block,
        std::move(smc_monitor));
    ASSERT_NOT_NULL(device);
    EXPECT_OK(device->Init());

    hiu_mock_.VerifyAll();
    power_mock_.VerifyAll();
    memory_pd_mock_.VerifyAll();
    EXPECT_OK(reset()->VerifyAll());

    loop_.Shutdown();
  }

  mock_registers::MockRegisters* reset() { return reset_mock_.get(); }

  ddk_mock::MockMmioRegRegion hiu_mock_;
  ddk_mock::MockMmioRegRegion power_mock_;
  ddk_mock::MockMmioRegRegion memory_pd_mock_;

  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::unique_ptr<mock_registers::MockRegisters> reset_mock_;
};

TEST(AmlNnaTest, InitT931) {
  MockRegistersInternal mock_regs;

  mock_regs.power_mock_[0x3a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFCFFFF);
  mock_regs.power_mock_[0x3b * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFCFFFF);

  mock_regs.memory_pd_mock_[0x43 * sizeof(uint32_t)].ExpectWrite(0);
  mock_regs.memory_pd_mock_[0x44 * sizeof(uint32_t)].ExpectWrite(0);

  mock_regs.reset()->ExpectWrite<uint32_t>(0x88, 1 << 12, 0);
  mock_regs.reset()->ExpectWrite<uint32_t>(0x88, 1 << 12, 1 << 12);

  mock_regs.hiu_mock_[0x72 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x700);
  mock_regs.hiu_mock_[0x72 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x7000000);

  ASSERT_NO_FATAL_FAILURE(mock_regs.CreateDeviceAndVerify(T931NnaBlock));
}

TEST(AmlNnaTest, InitS905d3) {
  MockRegistersInternal mock_regs;

  mock_regs.power_mock_[0x3a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFEFFFF);
  mock_regs.power_mock_[0x3b * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFEFFFF);

  mock_regs.memory_pd_mock_[0x46 * sizeof(uint32_t)].ExpectWrite(0);
  mock_regs.memory_pd_mock_[0x47 * sizeof(uint32_t)].ExpectWrite(0);

  mock_regs.reset()->ExpectWrite<uint32_t>(0x88, 1 << 12, 0);
  mock_regs.reset()->ExpectWrite<uint32_t>(0x88, 1 << 12, 1 << 12);

  mock_regs.hiu_mock_[0x72 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x700);
  mock_regs.hiu_mock_[0x72 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x7000000);

  ASSERT_NO_FATAL_FAILURE(mock_regs.CreateDeviceAndVerify(S905d3NnaBlock));
}

}  // namespace aml_nna

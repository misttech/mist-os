// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpio.h"

#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>

#include <fbl/alloc_checker.h>
#include <mock-mmio-reg/mock-mmio-reg.h>

#include "a113-blocks.h"
#include "lib/device-protocol/pdev-fidl.h"
#include "s905d2-blocks.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

constexpr size_t kGpioRegSize = 0x100;
constexpr size_t kInterruptRegSize = 0x30;
constexpr size_t kInterruptRegOffset = 0x3c00;

}  // namespace

namespace gpio {

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
};

class FakeAmlGpio : public AmlGpio {
 public:
  static std::unique_ptr<FakeAmlGpio> Create(ddk::PDevFidl pdev, pdev_device_info info,
                                             ddk_mock::MockMmioRegRegion *mock_mmio_gpio,
                                             ddk_mock::MockMmioRegRegion *mock_mmio_gpio_a0,
                                             ddk_mock::MockMmioRegRegion *mock_mmio_interrupt,
                                             fdf_dispatcher_t *const dispatcher) {
    const AmlGpioBlock *gpio_blocks;
    const AmlGpioInterrupt *gpio_interrupt;
    size_t block_count;

    switch (info.pid) {
      case PDEV_PID_AMLOGIC_A113:
        gpio_blocks = a113_gpio_blocks;
        block_count = std::size(a113_gpio_blocks);
        gpio_interrupt = &a113_interrupt_block;
        break;
      case PDEV_PID_AMLOGIC_S905D2:
      case PDEV_PID_AMLOGIC_T931:
        // S905D2 and T931 are identical.
        gpio_blocks = s905d2_gpio_blocks;
        block_count = std::size(s905d2_gpio_blocks);
        gpio_interrupt = &s905d2_interrupt_block;
        break;
      default:
        zxlogf(ERROR, "FakeAmlGpio::Create: unsupported SOC PID %u", info.pid);
        return nullptr;
    }

    fbl::AllocChecker ac;

    fbl::Array<uint16_t> irq_info(new (&ac) uint16_t[info.irq_count], info.irq_count);
    if (!ac.check()) {
      zxlogf(ERROR, "FakeAmlGpio::Create: irq_info alloc failed");
      return nullptr;
    }
    for (uint32_t i = 0; i < info.irq_count; i++) {
      irq_info[i] = 255 + 1;
    }  // initialize irq_info

    fdf::MmioBuffer mmio_gpio(mock_mmio_gpio->GetMmioBuffer());
    fdf::MmioBuffer mmio_gpio_a0(mock_mmio_gpio_a0->GetMmioBuffer());
    fdf::MmioBuffer mmio_interrupt(mock_mmio_interrupt->GetMmioBuffer());

    FakeAmlGpio *device(new (&ac) FakeAmlGpio(std::move(pdev), std::move(mmio_gpio),
                                              std::move(mmio_gpio_a0), std::move(mmio_interrupt),
                                              gpio_blocks, gpio_interrupt, block_count,
                                              std::move(info), std::move(irq_info), dispatcher));
    if (!ac.check()) {
      zxlogf(ERROR, "FakeAmlGpio::Create: device object alloc failed");
      return nullptr;
    }

    return std::unique_ptr<FakeAmlGpio>(device);
  }

 private:
  explicit FakeAmlGpio(ddk::PDevFidl pdev, fdf::MmioBuffer mock_mmio_gpio,
                       fdf::MmioBuffer mock_mmio_gpio_a0, fdf::MmioBuffer mock_mmio_interrupt,
                       const AmlGpioBlock *gpio_blocks, const AmlGpioInterrupt *gpio_interrupt,
                       size_t block_count, pdev_device_info_t info, fbl::Array<uint16_t> irq_info,
                       fdf_dispatcher_t *dispatcher)
      : AmlGpio(std::move(pdev), std::move(mock_mmio_gpio), std::move(mock_mmio_gpio_a0),
                std::move(mock_mmio_interrupt), gpio_blocks, gpio_interrupt, block_count,
                std::move(info), std::move(irq_info), dispatcher) {}
};

zx::result<ddk::PDevFidl> StartPDev(
    async::Loop &incoming_loop, async_patterns::TestDispatcherBound<IncomingNamespace> &incoming) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_platform_device::Device>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }

  zx_status_t status = incoming_loop.StartThread("incoming-ns-thread");
  if (status != ZX_OK) {
    return zx::error(status);
  }

  incoming.SyncCall([server = std::move(endpoints->server)](IncomingNamespace *infra) mutable {
    infra->pdev_server.SetConfig(fake_pdev::FakePDevFidl::Config{
        .use_fake_irq = true,
    });
    infra->pdev_server.Connect(std::move(server));
  });
  return zx::ok(ddk::PDevFidl(std::move(endpoints->client)));
}

class A113AmlGpioTest : public zxtest::Test {
 public:
  A113AmlGpioTest() : runtime_(mock_ddk::GetDriverRuntime()) {}

  void SetUp() override {
    // make-up info, pid and irq_count needed for Create
    auto info = pdev_device_info{0, PDEV_PID_AMLOGIC_A113, 0, 2, 3, 0, 0, 0, {0}, "fake_info"};

    zx::result pdev_result = StartPDev(incoming_loop_, incoming_);
    ASSERT_OK(pdev_result);

    auto endpoints = fdf::CreateEndpoints<fuchsia_hardware_gpioimpl::GpioImpl>();
    ASSERT_TRUE(endpoints.is_ok());
    async::PostTask(fdf::Dispatcher::GetCurrent()->async_dispatcher(), [&]() {
      gpio_ = FakeAmlGpio::Create(std::move(pdev_result.value()), info, &mock_mmio_gpio_,
                                  &mock_mmio_gpio_a0_, &mock_mmio_interrupt_,
                                  fdf::Dispatcher::GetCurrent()->get());
      fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(endpoints->server),
                      gpio_.get());
    });
    runtime_->RunUntilIdle();

    client_.Bind(std::move(endpoints->client), fdf::Dispatcher::GetCurrent()->get());
  }

  void TearDown() override {
    runtime_->ResetQuit();
    async::PostTask(fdf::Dispatcher::GetCurrent()->async_dispatcher(), [&]() { gpio_.reset(); });
    runtime_->RunUntilIdle();
  }

 protected:
  std::unique_ptr<FakeAmlGpio> gpio_;
  ddk_mock::MockMmioRegRegion mock_mmio_gpio_{sizeof(uint32_t), kGpioRegSize};
  ddk_mock::MockMmioRegRegion mock_mmio_gpio_a0_{sizeof(uint32_t), kGpioRegSize};
  ddk_mock::MockMmioRegRegion mock_mmio_interrupt_{sizeof(uint32_t), kInterruptRegSize,
                                                   kInterruptRegOffset};
  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_;
  fdf::WireClient<fuchsia_hardware_gpioimpl::GpioImpl> client_;
};

class S905d2AmlGpioTest : public zxtest::Test {
 public:
  S905d2AmlGpioTest() : runtime_(mock_ddk::GetDriverRuntime()) {}

  void SetUp() override {
    // make-up info, pid and irq_count needed for Create
    auto info = pdev_device_info{0, PDEV_PID_AMLOGIC_S905D2, 0, 2, 3, 0, 0, 0, {0}, "fake_info"};
    zx::result pdev_result = StartPDev(incoming_loop_, incoming_);
    ASSERT_OK(pdev_result);

    auto endpoints = fdf::CreateEndpoints<fuchsia_hardware_gpioimpl::GpioImpl>();
    ASSERT_TRUE(endpoints.is_ok());
    async::PostTask(fdf::Dispatcher::GetCurrent()->async_dispatcher(), [&]() {
      gpio_ = FakeAmlGpio::Create(std::move(pdev_result.value()), info, &mock_mmio_gpio_,
                                  &mock_mmio_gpio_a0_, &mock_mmio_interrupt_,
                                  fdf::Dispatcher::GetCurrent()->get());
      fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(endpoints->server),
                      gpio_.get());
    });
    runtime_->RunUntilIdle();

    client_.Bind(std::move(endpoints->client), fdf::Dispatcher::GetCurrent()->get());
  }

  void TearDown() override {
    runtime_->ResetQuit();
    async::PostTask(fdf::Dispatcher::GetCurrent()->async_dispatcher(), [&]() { gpio_.reset(); });
    runtime_->RunUntilIdle();
  }

 protected:
  std::unique_ptr<FakeAmlGpio> gpio_;
  ddk_mock::MockMmioRegRegion mock_mmio_gpio_{sizeof(uint32_t), kGpioRegSize};
  ddk_mock::MockMmioRegRegion mock_mmio_gpio_a0_{sizeof(uint32_t), kGpioRegSize};
  ddk_mock::MockMmioRegRegion mock_mmio_interrupt_{sizeof(uint32_t), kInterruptRegSize,
                                                   kInterruptRegOffset};

  async::Loop incoming_loop_{&kAsyncLoopConfigNoAttachToCurrentThread};
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_{incoming_loop_.dispatcher(),
                                                                   std::in_place};
  std::shared_ptr<fdf_testing::DriverRuntime> runtime_;
  fdf::WireClient<fuchsia_hardware_gpioimpl::GpioImpl> client_;
};

// GpioImplSetAltFunction Tests
TEST_F(A113AmlGpioTest, A113AltMode1) {
  mock_mmio_gpio_[0x24 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000001);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0x00, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113AltMode2) {
  mock_mmio_gpio_[0x26 * sizeof(uint32_t)].ExpectRead(0x00000009 << 8).ExpectWrite(0x00000005 << 8);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0x12, 5).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113AltMode3) {
  mock_mmio_gpio_a0_[0x05 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000005 << 16);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0x56, 5).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2AltMode) {
  mock_mmio_gpio_[0xb6 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000001);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0x00, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, AltModeFail1) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0x00, 16).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, AltModeFail2) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0xFFFF, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplConfigIn Tests
TEST_F(A113AmlGpioTest, A113NoPull0) {
  mock_mmio_gpio_[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mock_mmio_gpio_[0x4a * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFE);  // pull_en
                                 //
  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113NoPullMid) {
  mock_mmio_gpio_[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mock_mmio_gpio_[0x4a * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFBFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0x12, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113NoPullHigh) {
  mock_mmio_gpio_a0_[0x08 * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_a0_[0x0b * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFF);  // pull
  mock_mmio_gpio_a0_[0x0b * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFEFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0x56, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2NoPull0) {
  mock_mmio_gpio_[0x1c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_[0x3e * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mock_mmio_gpio_[0x4c * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFE);  // pull_en

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0x0, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2PullUp) {
  mock_mmio_gpio_[0x10 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_[0x3a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mock_mmio_gpio_[0x48 * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0x21, fuchsia_hardware_gpio::GpioFlags::kPullUp)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2PullDown) {
  mock_mmio_gpio_[0x10 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_[0x3a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFE);  // pull
  mock_mmio_gpio_[0x48 * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0x20, fuchsia_hardware_gpio::GpioFlags::kPullDown)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113NoPullFail) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0xFFFF, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_FALSE(result->is_ok());
        runtime_->Quit();
      });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplConfigOut Tests
TEST_F(A113AmlGpioTest, A113Out) {
  mock_mmio_gpio_[0x0d * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFFF);  // output
  mock_mmio_gpio_[0x0c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFB);  // oen

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->ConfigOut(0x19, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplRead Tests
TEST_F(A113AmlGpioTest, A113Read) {
  mock_mmio_gpio_[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mock_mmio_gpio_[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mock_mmio_gpio_[0x4a * sizeof(uint32_t)]
      .ExpectRead(0xFFFFFFFF)
      .ExpectWrite(0xFFFFFFDF);  // pull_en

  mock_mmio_gpio_[0x14 * sizeof(uint32_t)].ExpectRead(0x00000020);  // read 0x01.
  mock_mmio_gpio_[0x14 * sizeof(uint32_t)].ExpectRead(0x00000000);  // read 0x00.
  mock_mmio_gpio_[0x14 * sizeof(uint32_t)].ExpectRead(0x00000020);  // read 0x01.

  fdf::Arena arena('GPIO');

  client_.buffer(arena)
      ->ConfigIn(5, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
      });

  client_.buffer(arena)->Read(5).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x01);
  });
  client_.buffer(arena)->Read(5).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x00);
  });
  client_.buffer(arena)->Read(5).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->value, 0x01);
    runtime_->Quit();
  });

  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplWrite Tests
TEST_F(A113AmlGpioTest, A113Write) {
  mock_mmio_gpio_[0x13 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // write
  mock_mmio_gpio_[0x13 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFBFFF);  // write
  mock_mmio_gpio_[0x13 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // write

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->Write(14, 200).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)->Write(14, 0).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)->Write(14, 92).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });

  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplGetInterrupt Tests
TEST_F(A113AmlGpioTest, A113GetInterruptFail) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetInterrupt(0xFFFF, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113GetInterrupt) {
  mock_mmio_interrupt_[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  mock_mmio_interrupt_[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00010001);
  mock_mmio_interrupt_[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);
  zx::interrupt out_int;

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetInterrupt(0x0B, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplReleaseInterrupt Tests
TEST_F(A113AmlGpioTest, A113ReleaseInterruptFail) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->ReleaseInterrupt(0x66).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(A113AmlGpioTest, A113ReleaseInterrupt) {
  mock_mmio_interrupt_[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  mock_mmio_interrupt_[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00010001);
  mock_mmio_interrupt_[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->GetInterrupt(0x0B, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)->ReleaseInterrupt(0x0B).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_->Quit();
  });

  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplSetPolarity Tests
TEST_F(A113AmlGpioTest, A113InterruptSetPolarityEdge) {
  mock_mmio_interrupt_[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  mock_mmio_interrupt_[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00010001);
  mock_mmio_interrupt_[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);
  mock_mmio_interrupt_[0x3c20 * sizeof(uint32_t)]
      .ExpectRead(0x00010001)
      .ExpectWrite(0x00000001);  // polarity + for any edge.

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->GetInterrupt(0x0B, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetPolarity(0x0B, fuchsia_hardware_gpio::GpioPolarity::kHigh)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_->Quit();
      });

  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

// GpioImplSetDriveStrength Tests
TEST_F(A113AmlGpioTest, A113SetDriveStrength) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetDriveStrength(0x87, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2SetDriveStrength) {
  mock_mmio_gpio_a0_[0x08 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFB);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetDriveStrength(0x62, 3000).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->actual_ds_ua, 3000);
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2GetDriveStrength) {
  mock_mmio_gpio_a0_[0x08 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFB);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetDriveStrength(0x62).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->result_ua, 3000);
    runtime_->Quit();
  });
  runtime_->Run();

  mock_mmio_gpio_.VerifyAll();
  mock_mmio_gpio_a0_.VerifyAll();
  mock_mmio_interrupt_.VerifyAll();
}

}  // namespace gpio

int main(int argc, char **argv) { return RUN_ALL_TESTS(argc, argv); }

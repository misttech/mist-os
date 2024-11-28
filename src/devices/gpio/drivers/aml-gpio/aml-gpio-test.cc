// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpio.h"

#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>
#include <mock-mmio-reg/mock-mmio-reg.h>

namespace {

constexpr size_t kGpioRegSize = 0x100;
constexpr size_t kInterruptRegSize = 0x30;
constexpr size_t kInterruptRegOffset = 0x3c00;

}  // namespace

namespace gpio {
template <uint32_t kPid>
class FakePlatformDevice : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }
  void SetExpectedInterruptFlags(uint32_t flags) { expected_interrupt_flags_ = flags; }

 private:
  void GetMmioById(fuchsia_hardware_platform_device::wire::DeviceGetMmioByIdRequest* request,
                   GetMmioByIdCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetMmioByName(fuchsia_hardware_platform_device::wire::DeviceGetMmioByNameRequest* request,
                     GetMmioByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetInterruptById(
      fuchsia_hardware_platform_device::wire::DeviceGetInterruptByIdRequest* request,
      GetInterruptByIdCompleter::Sync& completer) override {
    EXPECT_EQ(request->flags, expected_interrupt_flags_);
    zx::interrupt irq;
    if (zx_status_t status = zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq);
        status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    // Trigger the interrupt so that the test can wait on it.
    irq.trigger(0, zx::time_boot());
    completer.ReplySuccess(std::move(irq));
  }

  void GetInterruptByName(
      fuchsia_hardware_platform_device::wire::DeviceGetInterruptByNameRequest* request,
      GetInterruptByNameCompleter::Sync& completer) override {
    zx::interrupt irq;
    if (zx_status_t status = zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &irq);
        status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    irq.trigger(0, zx::time_boot());
    completer.ReplySuccess(std::move(irq));
  }

  void GetBtiById(fuchsia_hardware_platform_device::wire::DeviceGetBtiByIdRequest* request,
                  GetBtiByIdCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetBtiByName(fuchsia_hardware_platform_device::wire::DeviceGetBtiByNameRequest* request,
                    GetBtiByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetSmcById(fuchsia_hardware_platform_device::wire::DeviceGetSmcByIdRequest* request,
                  GetSmcByIdCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override {
    fidl::Arena arena;
    auto info = fuchsia_hardware_platform_device::wire::NodeDeviceInfo::Builder(arena);
    // Report kIrqCount IRQs even though we don't check on GetInterruptX calls.
    static constexpr size_t kIrqCount = 3;
    completer.ReplySuccess(info.vid(PDEV_VID_AMLOGIC).pid(kPid).irq_count(kIrqCount).Build());
  }

  void GetSmcByName(fuchsia_hardware_platform_device::wire::DeviceGetSmcByNameRequest* request,
                    GetSmcByNameCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetMetadata(fuchsia_hardware_platform_device::wire::DeviceGetMetadataRequest* request,
                   GetMetadataCompleter::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void GetMetadata2(fuchsia_hardware_platform_device::wire::DeviceGetMetadata2Request* request,
                    GetMetadata2Completer::Sync& completer) override {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> bindings_;
  uint32_t expected_interrupt_flags_ = ZX_INTERRUPT_MODE_EDGE_HIGH;
};

class TestAmlGpioDriver : public AmlGpioDriver {
 public:
  TestAmlGpioDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : AmlGpioDriver(std::move(start_args), std::move(dispatcher)) {}

  static DriverRegistration GetDriverRegistration() {
    // Use a custom DriverRegistration to create the DUT. Without this, the non-test implementation
    // will be used by default.
    return FUCHSIA_DRIVER_REGISTRATION_V1(fdf_internal::DriverServer<TestAmlGpioDriver>::initialize,
                                          fdf_internal::DriverServer<TestAmlGpioDriver>::destroy);
  }

  ddk_mock::MockMmioRegRegion& mmio() { return mock_mmio_gpio_; }
  ddk_mock::MockMmioRegRegion& ao_mmio() { return mock_mmio_gpio_ao_; }
  ddk_mock::MockMmioRegRegion& interrupt_mmio() { return mock_mmio_interrupt_; }

 protected:
  fpromise::promise<fdf::MmioBuffer, zx_status_t> MapMmio(
      fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id) override {
    switch (mmio_id) {
      case 0:
        return fpromise::make_promise([&]() -> fpromise::result<fdf::MmioBuffer, zx_status_t> {
          return fpromise::ok(mock_mmio_gpio_.GetMmioBuffer());
        });
      case 1:
        return fpromise::make_promise([&]() -> fpromise::result<fdf::MmioBuffer, zx_status_t> {
          return fpromise::ok(mock_mmio_gpio_ao_.GetMmioBuffer());
        });
      case 2:
        return fpromise::make_promise([&]() -> fpromise::result<fdf::MmioBuffer, zx_status_t> {
          return fpromise::ok(mock_mmio_interrupt_.GetMmioBuffer());
        });
      default:
        return fpromise::make_promise([&]() -> fpromise::result<fdf::MmioBuffer, zx_status_t> {
          return fpromise::error(ZX_ERR_BAD_STATE);
        });
    }
  }

 private:
  ddk_mock::MockMmioRegRegion mock_mmio_gpio_{sizeof(uint32_t), kGpioRegSize};
  ddk_mock::MockMmioRegRegion mock_mmio_gpio_ao_{sizeof(uint32_t), kGpioRegSize};
  ddk_mock::MockMmioRegRegion mock_mmio_interrupt_{sizeof(uint32_t), kInterruptRegSize,
                                                   kInterruptRegOffset};
};

template <uint32_t kPid>
struct IncomingNamespace {
  FakePlatformDevice<kPid> platform_device;
};

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
template <uint32_t kPid>
class AmlGpioTest : public testing::Test {
 public:
  AmlGpioTest() : node_server_("root"), dut_(TestAmlGpioDriver::GetDriverRegistration()) {}

  void SetUp() override {
    zx::result start_args = node_server_.CreateStartArgsAndServe();
    ASSERT_TRUE(start_args.is_ok());

    driver_outgoing_ = std::move(start_args->outgoing_directory_client);

    {
      zx::result result =
          test_environment_.Initialize(std::move(start_args->incoming_directory_server));
      ASSERT_TRUE(result.is_ok());
    }

    {
      zx::result result = test_environment_.incoming_directory()
                              .AddService<fuchsia_hardware_platform_device::Service>(
                                  incoming_.platform_device.GetInstanceHandler());
      ASSERT_TRUE(result.is_ok());
    }

    zx::result start_result =
        runtime_.RunToCompletion(dut_.Start(std::move(start_args->start_args)));
    ASSERT_TRUE(start_result.is_ok());

    ASSERT_NE(node_server_.children().find("aml-gpio"), node_server_.children().cend());

    // Connect to the driver through its outgoing directory and get a pinimpl client.
    auto svc_endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();

    EXPECT_EQ(fdio_open3_at(driver_outgoing_.handle()->get(), "/svc",
                            static_cast<uint64_t>(fuchsia_io::wire::Flags::kProtocolDirectory),
                            svc_endpoints.server.TakeChannel().release()),
              ZX_OK);

    zx::result pinimpl_client_end =
        fdf::internal::DriverTransportConnect<fuchsia_hardware_pinimpl::Service::Device>(
            svc_endpoints.client, component::kDefaultInstance);
    ASSERT_TRUE(pinimpl_client_end.is_ok());

    client_.Bind(*std::move(pinimpl_client_end), fdf::Dispatcher::GetCurrent()->get());
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
    EXPECT_EQ(prepare_stop_result.status_value(), ZX_OK);
    EXPECT_TRUE(dut_.Stop().is_ok());
  }

 protected:
  void CheckA113GetInterrupt(uint32_t expected_kernel_interrupt_flags,
                             uint32_t expected_register_polarity_values) {
    incoming_.platform_device.SetExpectedInterruptFlags(expected_kernel_interrupt_flags);
    interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(expected_register_polarity_values);

    // Modify IRQ index for IRQ pin.
    interrupt_mmio()[0x3c21 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000048);
    // Interrupt select filter.
    interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

    zx::interrupt out_int;
    fdf::Arena arena('GPIO');
    client_.buffer(arena)->GetInterrupt(0x0B, {}).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
      runtime_.Quit();
    });
    runtime_.Run();

    mmio().VerifyAll();
    ao_mmio().VerifyAll();
    interrupt_mmio().VerifyAll();
  }
  ddk_mock::MockMmioRegRegion& mmio() { return dut_->mmio(); }
  ddk_mock::MockMmioRegRegion& ao_mmio() { return dut_->ao_mmio(); }
  ddk_mock::MockMmioRegRegion& interrupt_mmio() { return dut_->interrupt_mmio(); }
  IncomingNamespace<kPid>& incoming() { return incoming_; }

  fdf_testing::DriverRuntime runtime_;
  fdf::WireClient<fuchsia_hardware_pinimpl::PinImpl> client_;

 private:
  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
  fdf::UnownedSynchronizedDispatcher background_dispatcher_;
  IncomingNamespace<kPid> incoming_;
  fdf_testing::TestNode node_server_;
  fdf_testing::internal::TestEnvironment test_environment_;
  fdf_testing::internal::DriverUnderTest<TestAmlGpioDriver> dut_;
};

using A113AmlGpioTest = AmlGpioTest<PDEV_PID_AMLOGIC_A113>;
using S905d2AmlGpioTest = AmlGpioTest<PDEV_PID_AMLOGIC_S905D2>;

// PinImplSetAltFunction Tests
TEST_F(A113AmlGpioTest, A113AltMode1) {
  mmio()[0x24 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000001);

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).function(1).Build();
  client_.buffer(arena)->Configure(0x00, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113AltMode2) {
  mmio()[0x26 * sizeof(uint32_t)].ExpectRead(0x00000009 << 8).ExpectWrite(0x00000005 << 8);

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).function(5).Build();
  client_.buffer(arena)->Configure(0x12, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113AltMode3) {
  ao_mmio()[0x05 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000005 << 16);

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).function(5).Build();
  client_.buffer(arena)->Configure(0x56, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2AltMode) {
  mmio()[0xb6 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000001);

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).function(1).Build();
  client_.buffer(arena)->Configure(0x00, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, AltModeFail1) {
  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).function(16).Build();
  client_.buffer(arena)->Configure(0x00, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, AltModeFail2) {
  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).function(1).Build();
  client_.buffer(arena)->Configure(0xFFFF, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// PinImplConfigIn Tests
TEST_F(A113AmlGpioTest, A113NoPull0) {
  mmio()[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x4a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFE);  // pull_en

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kNone)
                    .Build();
  client_.buffer(arena)->Configure(0, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetBufferMode(0, fuchsia_hardware_gpio::BufferMode::kInput)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113NoPullMid) {
  mmio()[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x4a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFBFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kNone)
                    .Build();
  client_.buffer(arena)->Configure(0x12, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetBufferMode(0x12, fuchsia_hardware_gpio::BufferMode::kInput)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113NoPullHigh) {
  ao_mmio()[0x08 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  ao_mmio()[0x0b * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  ao_mmio()[0x0b * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFEFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kNone)
                    .Build();
  client_.buffer(arena)->Configure(0x56, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetBufferMode(0x56, fuchsia_hardware_gpio::BufferMode::kInput)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2NoPull0) {
  mmio()[0x1c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3e * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x4c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFE);  // pull_en

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kNone)
                    .Build();
  client_.buffer(arena)->Configure(0x0, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetBufferMode(0x0, fuchsia_hardware_gpio::BufferMode::kInput)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2PullUp) {
  mmio()[0x10 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x48 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kUp)
                    .Build();
  client_.buffer(arena)->Configure(0x21, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetBufferMode(0x21, fuchsia_hardware_gpio::BufferMode::kInput)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2PullDown) {
  mmio()[0x10 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFE);  // pull
  mmio()[0x48 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull_en

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kDown)
                    .Build();
  client_.buffer(arena)->Configure(0x20, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  client_.buffer(arena)
      ->SetBufferMode(0x20, fuchsia_hardware_gpio::BufferMode::kInput)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113NoPullFail) {
  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kNone)
                    .Build();
  client_.buffer(arena)->Configure(0xFFFF, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// PinImplConfigOut Tests
TEST_F(A113AmlGpioTest, A113Out) {
  mmio()[0x0d * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // output
  mmio()[0x0c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFB);  // oen

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->SetBufferMode(0x19, fuchsia_hardware_gpio::BufferMode::kOutputHigh)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// PinImplRead Tests
TEST_F(A113AmlGpioTest, A113Read) {
  mmio()[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x4a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFDF);  // pull_en

  mmio()[0x14 * sizeof(uint32_t)].ExpectRead(0x00000020);  // read 0x01.
  mmio()[0x14 * sizeof(uint32_t)].ExpectRead(0x00000000);  // read 0x00.
  mmio()[0x14 * sizeof(uint32_t)].ExpectRead(0x00000020);  // read 0x01.

  fdf::Arena arena('GPIO');

  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                    .pull(fuchsia_hardware_pin::Pull::kNone)
                    .Build();
  client_.buffer(arena)->Configure(5, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  client_.buffer(arena)
      ->SetBufferMode(5, fuchsia_hardware_gpio::BufferMode::kInput)
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
    runtime_.Quit();
  });

  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// PinImplGetInterrupt Tests
TEST_F(A113AmlGpioTest, A113GetInterruptFail) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetInterrupt(0xFFFF, {}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113GetInterruptEdgeLow) {
  // Request edge low polarity. Expect the kernel polarity to be converted to edge high.
  CheckA113GetInterrupt(ZX_INTERRUPT_MODE_EDGE_HIGH, 0x0001'0001);
}

TEST_F(A113AmlGpioTest, A113GetInterruptEdgeHigh) {
  // Request edge high polarity. Expect the kernel polarity to stay as edge high.
  CheckA113GetInterrupt(ZX_INTERRUPT_MODE_EDGE_HIGH, 0x0000'0001);
}

TEST_F(A113AmlGpioTest, A113GetInterruptLevelLow) {
  // Request level low polarity. Expect the kernel polarity to be converted to level high.
  CheckA113GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_HIGH, 0x0001'0000);
}

TEST_F(A113AmlGpioTest, A113GetInterruptLevelHigh) {
  // Request level high polarity. Expect the kernel polarity to stay as level high.
  CheckA113GetInterrupt(ZX_INTERRUPT_MODE_LEVEL_HIGH, 0x0000'0000);
}

// PinImplReleaseInterrupt Tests
TEST_F(A113AmlGpioTest, A113ReleaseInterruptFail) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->ReleaseInterrupt(0x66).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113ReleaseInterrupt) {
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00010001);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->GetInterrupt(0x0B, {}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());

    client_.buffer(arena)->ReleaseInterrupt(0x0B).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
      runtime_.Quit();
    });
  });

  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// PinImplConfigureInterrupt Tests
TEST_F(A113AmlGpioTest, A113ConfigureInterrupt) {
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00010001);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)]
      .ExpectRead(0x00010001)
      .ExpectWrite(0x00000001)  // polarity + for any edge.
      .ExpectRead(0x00000001)
      .ExpectWrite(0x00000000)
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00010000)
      .ExpectRead(0x00010000)
      .ExpectWrite(0x00010001);

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->GetInterrupt(0x0B, {}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());

    auto edge_high = fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena)
                         .mode(fuchsia_hardware_gpio::InterruptMode::kEdgeHigh)
                         .Build();
    client_.buffer(arena)->ConfigureInterrupt(0x0B, edge_high).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
    });

    auto level_high = fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena)
                          .mode(fuchsia_hardware_gpio::InterruptMode::kLevelHigh)
                          .Build();
    client_.buffer(arena)->ConfigureInterrupt(0x0B, level_high).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
    });

    auto level_low = fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena)
                         .mode(fuchsia_hardware_gpio::InterruptMode::kLevelLow)
                         .Build();
    client_.buffer(arena)->ConfigureInterrupt(0x0B, level_low).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
    });

    auto edge_low = fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena)
                        .mode(fuchsia_hardware_gpio::InterruptMode::kEdgeLow)
                        .Build();
    client_.buffer(arena)->ConfigureInterrupt(0x0B, edge_low).Then([&](auto& result) {
      ASSERT_TRUE(result.ok());
      EXPECT_TRUE(result->is_ok());
      runtime_.Quit();
    });
  });

  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113ConfigureThenGetInterrupt) {
  fdf::Arena arena('GPIO');

  // Configuring the interrupt first should not result in any MMIO accesses.

  auto edge_high = fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena)
                       .mode(fuchsia_hardware_gpio::InterruptMode::kEdgeHigh)
                       .Build();
  client_.buffer(arena)->ConfigureInterrupt(0x0B, edge_high).ThenExactlyOnce([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  runtime_.RunUntilIdle();

  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000001);
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000048);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

  client_.buffer(arena)->GetInterrupt(0x0B, {}).ThenExactlyOnce([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  runtime_.RunUntilIdle();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// PinImplSetDriveStrength Tests
TEST_F(A113AmlGpioTest, A113SetDriveStrength) {
  fdf::Arena arena('GPIO');
  auto config =
      fuchsia_hardware_pin::wire::Configuration::Builder(arena).drive_strength_ua(2).Build();
  client_.buffer(arena)->Configure(0x87, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2SetDriveStrength) {
  ao_mmio()[0x08 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFB);

  fdf::Arena arena('GPIO');
  auto config =
      fuchsia_hardware_pin::wire::Configuration::Builder(arena).drive_strength_ua(3000).Build();
  client_.buffer(arena)->Configure(0x62, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    ASSERT_TRUE(result->value()->new_config.has_drive_strength_ua());
    EXPECT_EQ(result->value()->new_config.drive_strength_ua(), 3000);
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, S905d2GetDriveStrength) {
  ao_mmio()[0x08 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFB);

  fdf::Arena arena('GPIO');
  auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).Build();
  client_.buffer(arena)->Configure(0x62, config).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    ASSERT_TRUE(result->value()->new_config.has_drive_strength_ua());
    EXPECT_EQ(result->value()->new_config.drive_strength_ua(), 3000);
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(S905d2AmlGpioTest, ExtraInterruptOptions) {
  constexpr uint32_t kExtraInterruptOption = 0x20;

  incoming().platform_device.SetExpectedInterruptFlags(ZX_INTERRUPT_MODE_EDGE_HIGH |
                                                       kExtraInterruptOption);
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x0001'0001);

  // Modify IRQ index for IRQ pin.
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000017);
  // Interrupt select filter.
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

  // The mode is now set by ConfigureInterrupt(); the mode passed here should be ignored. Other
  // options should be passed through to platform-bus.
  const auto options = static_cast<fuchsia_hardware_gpio::InterruptOptions>(
      ZX_INTERRUPT_MODE_LEVEL_LOW | kExtraInterruptOption);

  zx::interrupt out_int;
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetInterrupt(0x0B, options).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, DestroyInterruptOnRelease) {
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00010001);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

  fdf::Arena arena('GPIO');

  zx::interrupt interrupt{};
  client_.buffer(arena)->GetInterrupt(0x0B, {}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    interrupt = std::move(result->value()->interrupt);
  });
  runtime_.RunUntilIdle();

  // FakePlatformDevice triggered the interrupt -- wait on it to make sure we have a working handle.
  EXPECT_EQ(interrupt.wait(nullptr), ZX_OK);

  client_.buffer(arena)->ReleaseInterrupt(0x0B).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  runtime_.RunUntilIdle();

  // AmlGpio should have called zx_interrupt_destroy in the interrupt for this pin.
  EXPECT_EQ(interrupt.wait(nullptr), ZX_ERR_CANCELED);

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

}  // namespace gpio

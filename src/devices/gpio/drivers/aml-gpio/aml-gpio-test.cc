// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpio.h"

#include <lib/ddk/platform-defs.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <mock-mmio-reg/mock-mmio-reg.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"

namespace {

constexpr size_t kGpioRegSize = 0x100;
constexpr size_t kInterruptRegSize = 0x30;
constexpr size_t kInterruptRegOffset = 0x3c00;

}  // namespace

namespace gpio {

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

struct IncomingNamespace {
  fake_pdev::FakePDevFidl pdev_server;
};

template <uint32_t kPid>
class AmlGpioTest : public zxtest::Test {
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
      incoming_.pdev_server.SetConfig(fake_pdev::FakePDevFidl::Config{
          .use_fake_irq = true,
          // make-up info, pid and irq_count needed for Create
          .device_info = pdev_device_info_t{0, kPid, 0, 2, 3, 0, 0, 0, {0}, "fake_info"},
      });
      auto instance_handler = incoming_.pdev_server.GetInstanceHandler(
          fdf::Dispatcher::GetCurrent()->async_dispatcher());
      zx::result result =
          test_environment_.incoming_directory()
              .AddService<fuchsia_hardware_platform_device::Service>(std::move(instance_handler));
      ASSERT_TRUE(result.is_ok());
    }

    zx::result start_result =
        runtime_.RunToCompletion(dut_.Start(std::move(start_args->start_args)));
    ASSERT_TRUE(start_result.is_ok());

    ASSERT_NE(node_server_.children().find("aml-gpio"), node_server_.children().cend());

    // Connect to the driver through its outgoing directory and get a gpioimpl client.
    zx::result svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    ASSERT_TRUE(svc_endpoints.is_ok());

    EXPECT_OK(fdio_open_at(driver_outgoing_.handle()->get(), "/svc",
                           static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                           svc_endpoints->server.TakeChannel().release()));

    zx::result gpioimpl_client_end =
        fdf::internal::DriverTransportConnect<fuchsia_hardware_gpioimpl::Service::Device>(
            svc_endpoints->client, component::kDefaultInstance);
    ASSERT_TRUE(gpioimpl_client_end.is_ok());

    client_.Bind(*std::move(gpioimpl_client_end), fdf::Dispatcher::GetCurrent()->get());
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
    EXPECT_OK(prepare_stop_result.status_value());
    EXPECT_TRUE(dut_.Stop().is_ok());
  }

 protected:
  ddk_mock::MockMmioRegRegion& mmio() { return dut_->mmio(); }
  ddk_mock::MockMmioRegRegion& ao_mmio() { return dut_->ao_mmio(); }
  ddk_mock::MockMmioRegRegion& interrupt_mmio() { return dut_->interrupt_mmio(); }

  fdf_testing::DriverRuntime runtime_;
  fdf::WireClient<fuchsia_hardware_gpioimpl::GpioImpl> client_;

 private:
  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
  fdf::UnownedSynchronizedDispatcher background_dispatcher_;
  IncomingNamespace incoming_;
  fdf_testing::TestNode node_server_;
  fdf_testing::TestEnvironment test_environment_;
  fdf_testing::DriverUnderTest<TestAmlGpioDriver> dut_;
};

using A113AmlGpioTest = AmlGpioTest<PDEV_PID_AMLOGIC_A113>;
using S905d2AmlGpioTest = AmlGpioTest<PDEV_PID_AMLOGIC_S905D2>;

// GpioImplSetAltFunction Tests
TEST_F(A113AmlGpioTest, A113AltMode1) {
  mmio()[0x24 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000001);

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetAltFunction(0x00, 1).Then([&](auto& result) {
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
  client_.buffer(arena)->SetAltFunction(0x12, 5).Then([&](auto& result) {
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
  client_.buffer(arena)->SetAltFunction(0x56, 5).Then([&](auto& result) {
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
  client_.buffer(arena)->SetAltFunction(0x00, 1).Then([&](auto& result) {
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
  client_.buffer(arena)->SetAltFunction(0x00, 16).Then([&](auto& result) {
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
  client_.buffer(arena)->SetAltFunction(0xFFFF, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// GpioImplConfigIn Tests
TEST_F(A113AmlGpioTest, A113NoPull0) {
  mmio()[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x4a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFE);  // pull_en

  fdf::Arena arena('GPIO');
  client_.buffer(arena)
      ->ConfigIn(0, fuchsia_hardware_gpio::GpioFlags::kNoPull)
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
  client_.buffer(arena)
      ->ConfigIn(0x12, fuchsia_hardware_gpio::GpioFlags::kNoPull)
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
  client_.buffer(arena)
      ->ConfigIn(0x56, fuchsia_hardware_gpio::GpioFlags::kNoPull)
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
  client_.buffer(arena)
      ->ConfigIn(0x0, fuchsia_hardware_gpio::GpioFlags::kNoPull)
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
  client_.buffer(arena)
      ->ConfigIn(0x21, fuchsia_hardware_gpio::GpioFlags::kPullUp)
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
  client_.buffer(arena)
      ->ConfigIn(0x20, fuchsia_hardware_gpio::GpioFlags::kPullDown)
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
  client_.buffer(arena)
      ->ConfigIn(0xFFFF, fuchsia_hardware_gpio::GpioFlags::kNoPull)
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_FALSE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// GpioImplConfigOut Tests
TEST_F(A113AmlGpioTest, A113Out) {
  mmio()[0x0d * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // output
  mmio()[0x0c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFB);  // oen

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->ConfigOut(0x19, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// GpioImplRead Tests
TEST_F(A113AmlGpioTest, A113Read) {
  mmio()[0x12 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // oen
  mmio()[0x3c * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // pull
  mmio()[0x4a * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFDF);  // pull_en

  mmio()[0x14 * sizeof(uint32_t)].ExpectRead(0x00000020);  // read 0x01.
  mmio()[0x14 * sizeof(uint32_t)].ExpectRead(0x00000000);  // read 0x00.
  mmio()[0x14 * sizeof(uint32_t)].ExpectRead(0x00000020);  // read 0x01.

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
    runtime_.Quit();
  });

  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// GpioImplWrite Tests
TEST_F(A113AmlGpioTest, A113Write) {
  mmio()[0x13 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // write
  mmio()[0x13 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFBFFF);  // write
  mmio()[0x13 * sizeof(uint32_t)].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFFFFFF);  // write

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
    runtime_.Quit();
  });

  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// GpioImplGetInterrupt Tests
TEST_F(A113AmlGpioTest, A113GetInterruptFail) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetInterrupt(0xFFFF, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_FALSE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

TEST_F(A113AmlGpioTest, A113GetInterrupt) {
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00010001);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);
  zx::interrupt out_int;

  fdf::Arena arena('GPIO');
  client_.buffer(arena)->GetInterrupt(0x0B, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

// GpioImplReleaseInterrupt Tests
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
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00010001);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->GetInterrupt(0x0B, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
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

// GpioImplSetPolarity Tests
TEST_F(A113AmlGpioTest, A113InterruptSetPolarityEdge) {
  interrupt_mmio()[0x3c21 * sizeof(uint32_t)]
      .ExpectRead(0x00000000)
      .ExpectWrite(0x00000048);  // modify
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00010001);
  interrupt_mmio()[0x3c23 * sizeof(uint32_t)].ExpectRead(0x00000000).ExpectWrite(0x00000007);
  interrupt_mmio()[0x3c20 * sizeof(uint32_t)]
      .ExpectRead(0x00010001)
      .ExpectWrite(0x00000001);  // polarity + for any edge.

  fdf::Arena arena('GPIO');

  client_.buffer(arena)->GetInterrupt(0x0B, ZX_INTERRUPT_MODE_EDGE_LOW).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());

    client_.buffer(arena)
        ->SetPolarity(0x0B, fuchsia_hardware_gpio::GpioPolarity::kHigh)
        .Then([&](auto& result) {
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

// GpioImplSetDriveStrength Tests
TEST_F(A113AmlGpioTest, A113SetDriveStrength) {
  fdf::Arena arena('GPIO');
  client_.buffer(arena)->SetDriveStrength(0x87, 2).Then([&](auto& result) {
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
  client_.buffer(arena)->SetDriveStrength(0x62, 3000).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->actual_ds_ua, 3000);
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
  client_.buffer(arena)->GetDriveStrength(0x62).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(result->value()->result_ua, 3000);
    runtime_.Quit();
  });
  runtime_.Run();

  mmio().VerifyAll();
  ao_mmio().VerifyAll();
  interrupt_mmio().VerifyAll();
}

}  // namespace gpio

// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-spi.h"

#include <endian.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fake-bti/bti.h>
#include <lib/zx/clock.h>
#include <lib/zx/vmo.h>
#include <zircon/errors.h>

#include <fake-mmio-reg/fake-mmio-reg.h>
#include <zxtest/zxtest.h>

#include "registers.h"
#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/gpio/testing/fake-gpio/fake-gpio.h"
#include "src/devices/registers/testing/mock-registers/mock-registers.h"

namespace spi {

class TestAmlSpiDriver : public AmlSpiDriver {
 public:
  TestAmlSpiDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : AmlSpiDriver(std::move(start_args), std::move(dispatcher)),
        mmio_region_(sizeof(uint32_t), 17) {}

  static DriverRegistration GetDriverRegistration() {
    // Use a custom DriverRegistration to create the DUT. Without this, the non-test implementation
    // will be used by default.
    return FUCHSIA_DRIVER_REGISTRATION_V1(fdf_internal::DriverServer<TestAmlSpiDriver>::initialize,
                                          fdf_internal::DriverServer<TestAmlSpiDriver>::destroy);
  }

  ddk_fake::FakeMmioRegRegion& mmio() { return mmio_region_; }

  uint32_t conreg() const { return conreg_; }
  uint32_t enhance_cntl() const { return enhance_cntl_; }
  uint32_t testreg() const { return testreg_; }

 protected:
  fpromise::promise<fdf::MmioBuffer, zx_status_t> MapMmio(
      fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id) override {
    return fpromise::make_promise([this]() -> fpromise::result<fdf::MmioBuffer, zx_status_t> {
      // Set the transfer complete bit so the driver doesn't get stuck waiting on the interrupt.
      mmio_region_[AML_SPI_STATREG].SetReadCallback(
          []() { return StatReg::Get().FromValue(0).set_tc(1).set_te(1).set_rr(1).reg_value(); });

      mmio_region_[AML_SPI_CONREG].SetWriteCallback([this](uint32_t value) { conreg_ = value; });
      mmio_region_[AML_SPI_CONREG].SetReadCallback([this]() { return conreg_; });
      mmio_region_[AML_SPI_ENHANCE_CNTL].SetWriteCallback(
          [this](uint32_t value) { enhance_cntl_ = value; });
      mmio_region_[AML_SPI_TESTREG].SetWriteCallback([this](uint32_t value) { testreg_ = value; });

      return fpromise::ok(mmio_region_.GetMmioBuffer());
    });
  }

 private:
  ddk_fake::FakeMmioRegRegion mmio_region_;
  uint32_t conreg_{};
  uint32_t enhance_cntl_{};
  uint32_t testreg_{};
};

class AmlSpiTest : public zxtest::Test {
 public:
  AmlSpiTest()
      : registers_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        node_server_("root"),
        dut_(TestAmlSpiDriver::GetDriverRegistration()) {}

  virtual void SetUpInterrupt(fake_pdev::FakePDevFidl::Config& config) {
    ASSERT_OK(zx::interrupt::create({}, 0, ZX_INTERRUPT_VIRTUAL, &interrupt_));
    zx::interrupt dut_interrupt;
    ASSERT_OK(interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dut_interrupt));
    config.irqs[0] = std::move(dut_interrupt);
    interrupt_.trigger(0, zx::clock::get_monotonic());
  }

  virtual void SetUpBti(fake_pdev::FakePDevFidl::Config& config) {}

  virtual bool SetupResetRegister() { return true; }

  virtual amlogic_spi::amlspi_config_t GetAmlSpiMetadata() {
    return amlogic_spi::amlspi_config_t{
        .bus_id = 0,
        .cs_count = 3,
        .cs = {5, 3, amlogic_spi::amlspi_config_t::kCsClientManaged},
        .clock_divider_register_value = 0,
        .use_enhanced_clock_mode = false,
    };
  }

  void SetUp() override {
    fake_pdev::FakePDevFidl::Config config;
    config.device_info = pdev_device_info_t{
        .mmio_count = 1,
        .irq_count = 1u,
    };

    SetUpInterrupt(config);
    SetUpBti(config);

    zx::result start_args = node_server_.CreateStartArgsAndServe();
    ASSERT_TRUE(start_args.is_ok());

    driver_outgoing_ = std::move(start_args->outgoing_directory_client);

    ASSERT_TRUE(
        test_environment_.Initialize(std::move(start_args->incoming_directory_server)).is_ok());

    start_args_ = std::move(start_args->start_args);

    pdev_server_.SetConfig(std::move(config));

    auto& directory = test_environment_.incoming_directory();

    auto result = directory.AddService<fuchsia_hardware_platform_device::Service>(
        pdev_server_.GetInstanceHandler(fdf::Dispatcher::GetCurrent()->async_dispatcher()), "pdev");
    ASSERT_TRUE(result.is_ok());

    const auto metadata = GetAmlSpiMetadata();
    EXPECT_OK(compat_.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &metadata, sizeof(metadata)));
    compat_.Init("pdev", {});
    EXPECT_OK(compat_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &directory));

    result = directory.AddService<fuchsia_hardware_gpio::Service>(gpio_.CreateInstanceHandler(),
                                                                  "gpio-cs-2");
    ASSERT_TRUE(result.is_ok());

    result = directory.AddService<fuchsia_hardware_gpio::Service>(gpio_.CreateInstanceHandler(),
                                                                  "gpio-cs-3");
    ASSERT_TRUE(result.is_ok());

    result = directory.AddService<fuchsia_hardware_gpio::Service>(gpio_.CreateInstanceHandler(),
                                                                  "gpio-cs-5");
    ASSERT_TRUE(result.is_ok());

    gpio_.SetCurrentState(fake_gpio::State{.polarity = fuchsia_hardware_gpio::GpioPolarity::kHigh,
                                           .sub_state = fake_gpio::WriteSubState{.value = 0}});
    gpio_.SetWriteCallback([this](fake_gpio::FakeGpio& gpio) {
      if (gpio_writes_.empty()) {
        EXPECT_FALSE(gpio_writes_.empty());
        return ZX_ERR_INTERNAL;
      }
      auto [status, value] = gpio_writes_.front();
      gpio_writes_.pop();
      if (status != ZX_OK) {
        EXPECT_EQ(value, gpio_.GetWriteValue());
      }
      return status;
    });

    if (SetupResetRegister()) {
      auto result =
          test_environment_.incoming_directory().AddService<fuchsia_hardware_registers::Service>(
              registers_.GetInstanceHandler(), "reset");
      ASSERT_TRUE(result.is_ok());
    }

    registers_.ExpectWrite<uint32_t>(0x1c, 1 << 1, 1 << 1);
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
    EXPECT_OK(prepare_stop_result.status_value());
    EXPECT_TRUE(dut_.Stop().is_ok());
  }

  void ExpectGpioWrite(zx_status_t status, uint8_t value) { gpio_writes_.emplace(status, value); }

  void VerifyGpioAndClear() {
    EXPECT_EQ(gpio_writes_.size(), 0);
    gpio_writes_ = {};
  }

  ddk_fake::FakeMmioRegRegion& mmio() { return dut_->mmio(); }
  bool ControllerReset() {
    zx_status_t status = registers_.VerifyAll();
    if (status == ZX_OK) {
      // Always keep a single expectation in the queue, that way we can verify when the controller
      // is not reset.
      registers_.ExpectWrite<uint32_t>(0x1c, 1 << 1, 1 << 1);
    }

    return status == ZX_OK;
  }

 protected:
  zx::result<fdf::ClientEnd<fuchsia_hardware_spiimpl::SpiImpl>> GetFidlClient() {
    // Connect to the driver through its outgoing directory and get a spiimpl client.
    zx::result svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    if (svc_endpoints.is_error()) {
      return svc_endpoints.take_error();
    }

    EXPECT_OK(fdio_open_at(driver_outgoing_.handle()->get(), "/svc",
                           static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                           svc_endpoints->server.TakeChannel().release()));

    return fdf::internal::DriverTransportConnect<fuchsia_hardware_spiimpl::Service::Device>(
        svc_endpoints->client, component::kDefaultInstance);
  }

  fdf_testing::DriverRuntime runtime_;
  fake_pdev::FakePDevFidl pdev_server_;
  mock_registers::MockRegisters registers_;
  std::queue<std::pair<zx_status_t, uint8_t>> gpio_writes_;
  fake_gpio::FakeGpio gpio_;
  fdf_testing::TestNode node_server_;
  fdf_testing::TestEnvironment test_environment_;
  fdf_testing::DriverUnderTest<TestAmlSpiDriver> dut_;
  fuchsia_driver_framework::DriverStartArgs start_args_;

 private:
  static constexpr amlogic_spi::amlspi_config_t kSpiConfig = {
      .bus_id = 0,
      .cs_count = 3,
      .cs = {5, 3, amlogic_spi::amlspi_config_t::kCsClientManaged},
      .clock_divider_register_value = 0,
      .use_enhanced_clock_mode = false,
  };

  zx::interrupt interrupt_;
  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
  compat::DeviceServer compat_;
};

zx_koid_t GetVmoKoid(const zx::vmo& vmo) {
  zx_info_handle_basic_t info = {};
  size_t actual = 0;
  size_t available = 0;
  zx_status_t status = vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), &actual, &available);
  if (status != ZX_OK || actual < 1) {
    return ZX_KOID_INVALID;
  }
  return info.koid;
}

TEST_F(AmlSpiTest, DdkLifecycle) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ASSERT_NE(node_server_.children().find("aml-spi-0"), node_server_.children().cend());
}

TEST_F(AmlSpiTest, ChipSelectCount) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());
  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)->GetChipSelectCount().Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->count, 3);
    runtime_.Quit();
  });
  runtime_.Run();
}

TEST_F(AmlSpiTest, Exchange) {
  uint8_t kTxData[] = {0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12};
  constexpr uint8_t kExpectedRxData[] = {0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_BYTES_EQ(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData));
        runtime_.Quit();
      });
  runtime_.Run();

  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ExchangeCsManagedByClient) {
  uint8_t kTxData[] = {0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12};
  constexpr uint8_t kExpectedRxData[] = {0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(2, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_BYTES_EQ(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData));
        runtime_.Quit();
      });
  runtime_.Run();

  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  // There should be no GPIO calls as the client manages CS for this device.
  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, RegisterVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  const zx_koid_t test_vmo_koid = GetVmoKoid(test_vmo);

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_error());
        });
  }

  spiimpl.buffer(arena)->UnregisterVmo(0, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    ASSERT_TRUE(result->is_ok());
    EXPECT_EQ(test_vmo_koid, GetVmoKoid(result->value()->vmo));
  });

  spiimpl.buffer(arena)->UnregisterVmo(0, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    runtime_.Quit();
  });
  runtime_.Run();
}

TEST_F(AmlSpiTest, TransmitVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  constexpr uint8_t kTxData[] = {0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 256, PAGE_SIZE - 256}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(test_vmo.write(kTxData, 512, sizeof(kTxData)));

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  spiimpl.buffer(arena)->TransmitVmo(0, {1, 256, sizeof(kTxData)}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ReceiveVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  constexpr uint8_t kExpectedRxData[] = {0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 256, PAGE_SIZE - 256},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)->ReceiveVmo(0, {1, 512, sizeof(kExpectedRxData)}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();

  uint8_t rx_buffer[sizeof(kExpectedRxData)];
  EXPECT_OK(test_vmo.read(rx_buffer, 768, sizeof(rx_buffer)));
  EXPECT_BYTES_EQ(rx_buffer, kExpectedRxData, sizeof(rx_buffer));

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ExchangeVmo) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  constexpr uint8_t kTxData[] = {0xef, 0xef, 0xef, 0xef, 0xef, 0xef, 0xef};
  constexpr uint8_t kExpectedRxData[] = {0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 256, PAGE_SIZE - 256},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(test_vmo.write(kTxData, 512, sizeof(kTxData)));

  spiimpl.buffer(arena)
      ->ExchangeVmo(0, {1, 256, sizeof(kTxData)}, {1, 512, sizeof(kExpectedRxData)})
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        EXPECT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  uint8_t rx_buffer[sizeof(kExpectedRxData)];
  EXPECT_OK(test_vmo.read(rx_buffer, 768, sizeof(rx_buffer)));
  EXPECT_BYTES_EQ(rx_buffer, kExpectedRxData, sizeof(rx_buffer));

  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, TransfersOutOfRange) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(1, 1, {std::move(vmo), PAGE_SIZE - 4, 4},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 0, 2}, {1, 2, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 0, 2}, {1, 3, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 3, 2}, {1, 0, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ExchangeVmo(1, {1, 0, 3}, {1, 2, 3}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)->TransmitVmo(1, {1, 0, 4}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 0, 5}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 3, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 4, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->TransmitVmo(1, {1, 5, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);
  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 0, 4}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);
  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 3, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 3, 2}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 4, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });
  spiimpl.buffer(arena)->ReceiveVmo(1, {1, 5, 1}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    runtime_.Quit();
  });
  runtime_.Run();

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, VmoBadRights) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, 256}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, 256},
                      SharedVmoRight::kRead | SharedVmoRight::kWrite)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)->ExchangeVmo(0, {1, 0, 128}, {2, 128, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });
  spiimpl.buffer(arena)->ExchangeVmo(0, {2, 0, 128}, {1, 128, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->error_value(), ZX_ERR_ACCESS_DENIED);
  });
  spiimpl.buffer(arena)->ExchangeVmo(0, {1, 0, 128}, {1, 128, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->error_value(), ZX_ERR_ACCESS_DENIED);
  });
  spiimpl.buffer(arena)->ReceiveVmo(0, {1, 0, 128}).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_EQ(result->error_value(), ZX_ERR_ACCESS_DENIED);
    runtime_.Quit();
  });
  runtime_.Run();

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, Exchange64BitWords) {
  uint8_t kTxData[] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd,
      0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba, 0xf1, 0x49, 0x00,
  };
  constexpr uint8_t kExpectedRxData[] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
  };

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  // First (and only) word of kExpectedRxData with bytes swapped.
  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea2b'8f8f; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  fdf::Arena arena('TEST');

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_BYTES_EQ(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData));
        runtime_.Quit();
      });
  runtime_.Run();

  // Last word of kTxData with bytes swapped.
  EXPECT_EQ(tx_data, 0xbaf1'4900);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, Exchange64Then8BitWords) {
  uint8_t kTxData[] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb,
      0xbd, 0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba,
  };
  constexpr uint8_t kExpectedRxData[] = {
      0x00, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00,
      0xea, 0x00, 0x00, 0x00, 0xea, 0xea, 0xea, 0xea, 0xea, 0xea,
  };

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  fdf::Arena arena('TEST');

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(kTxData, sizeof(kTxData)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(kExpectedRxData));
        EXPECT_BYTES_EQ(result->value()->rxdata.data(), kExpectedRxData, sizeof(kExpectedRxData));
        runtime_.Quit();
      });
  runtime_.Run();

  EXPECT_EQ(tx_data, 0xba);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ExchangeResetsController) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  fdf::Arena arena('TEST');

  uint8_t buf[17] = {};

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 17))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 17);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  // Controller should be reset because a 64-bit transfer was preceded by a transfer of an odd
  // number of bytes.
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 16))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 16);
        EXPECT_TRUE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 3))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 3);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 6))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 6);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 8))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 8);
        EXPECT_TRUE(ControllerReset());
        runtime_.Quit();
      });
  runtime_.Run();

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ReleaseVmos) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fdf::Arena arena('TEST');

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  spiimpl.buffer(arena)->UnregisterVmo(0, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
  });

  // Release VMO 1 and make sure that a subsequent call to unregister it fails.
  EXPECT_TRUE(spiimpl.buffer(arena)->ReleaseRegisteredVmos(0).ok());

  spiimpl.buffer(arena)->UnregisterVmo(0, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
  });

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });
  }

  // Release both VMOs and make sure that they can be registered again.
  EXPECT_TRUE(spiimpl.buffer(arena)->ReleaseRegisteredVmos(0).ok());

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 1, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
        });

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl.buffer(arena)
        ->RegisterVmo(0, 2, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
          runtime_.Quit();
        });
  }

  runtime_.Run();
}

TEST_F(AmlSpiTest, ReleaseVmosAfterClientsUnbind) {
  using fuchsia_hardware_sharedmemory::SharedVmoRight;

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client1 = GetFidlClient();
  ASSERT_TRUE(spiimpl_client1.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl1(*std::move(spiimpl_client1),
                                                              fdf::Dispatcher::GetCurrent()->get());

  fdf::Arena arena('TEST');

  // Register three VMOs through the first client.
  for (uint32_t i = 1; i <= 3; i++) {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    spiimpl1.buffer(arena)
        ->RegisterVmo(0, i, {std::move(vmo), 0, PAGE_SIZE}, SharedVmoRight::kRead)
        .Then([&](auto& result) {
          ASSERT_TRUE(result.ok());
          EXPECT_TRUE(result->is_ok());
          runtime_.Quit();
        });
    runtime_.Run();
    runtime_.ResetQuit();
  }

  auto spiimpl_client2 = GetFidlClient();
  ASSERT_TRUE(spiimpl_client2.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl2(*std::move(spiimpl_client2),
                                                              fdf::Dispatcher::GetCurrent()->get());

  // The second client should be able to see the registered VMOs.
  spiimpl2.buffer(arena)->UnregisterVmo(0, 1).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();
  runtime_.ResetQuit();

  // Unbind the first client.
  EXPECT_TRUE(spiimpl1.UnbindMaybeGetEndpoint().is_ok());
  runtime_.RunUntilIdle();

  // The VMOs registered by the first client should remain.
  spiimpl2.buffer(arena)->UnregisterVmo(0, 2).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_ok());
    runtime_.Quit();
  });
  runtime_.Run();
  runtime_.ResetQuit();

  // Unbind the second client, then connect a third client.
  EXPECT_TRUE(spiimpl2.UnbindMaybeGetEndpoint().is_ok());
  runtime_.RunUntilIdle();

  auto spiimpl_client3 = GetFidlClient();
  ASSERT_TRUE(spiimpl_client3.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl3(*std::move(spiimpl_client3),
                                                              fdf::Dispatcher::GetCurrent()->get());

  // All registered VMOs should have been released after the second client unbound.
  spiimpl3.buffer(arena)->UnregisterVmo(0, 3).Then([&](auto& result) {
    ASSERT_TRUE(result.ok());
    EXPECT_TRUE(result->is_error());
    runtime_.Quit();
  });
  runtime_.Run();
}

class AmlSpiNormalClockModeTest : public AmlSpiTest {
 public:
  amlogic_spi::amlspi_config_t GetAmlSpiMetadata() override {
    return amlogic_spi::amlspi_config_t{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0x5,
        .use_enhanced_clock_mode = false,

    };
  }
};

TEST_F(AmlSpiNormalClockModeTest, Test) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto conreg = ConReg::Get().FromValue(dut_->conreg());
  auto enhanced_cntl = EnhanceCntl::Get().FromValue(dut_->enhance_cntl());
  auto testreg = TestReg::Get().FromValue(dut_->testreg());

  EXPECT_EQ(conreg.data_rate(), 0x5);
  EXPECT_EQ(conreg.drctl(), 0);
  EXPECT_EQ(conreg.ssctl(), 0);
  EXPECT_EQ(conreg.smc(), 0);
  EXPECT_EQ(conreg.xch(), 0);
  EXPECT_EQ(conreg.mode(), ConReg::kModeMaster);
  EXPECT_EQ(conreg.en(), 1);

  EXPECT_EQ(enhanced_cntl.reg_value(), 0);

  EXPECT_EQ(testreg.dlyctl(), 0x15);
  EXPECT_EQ(testreg.clk_free_en(), 1);
}

class AmlSpiEnhancedClockModeTest : public AmlSpiTest {
 public:
  amlogic_spi::amlspi_config_t GetAmlSpiMetadata() override {
    return amlogic_spi::amlspi_config_t{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0xa5,
        .use_enhanced_clock_mode = true,
        .delay_control = 0b00'11'00,
    };
  }
};

TEST_F(AmlSpiEnhancedClockModeTest, Test) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto conreg = ConReg::Get().FromValue(dut_->conreg());
  auto enhanced_cntl = EnhanceCntl::Get().FromValue(dut_->enhance_cntl());
  auto testreg = TestReg::Get().FromValue(dut_->testreg());

  EXPECT_EQ(conreg.data_rate(), 0);
  EXPECT_EQ(conreg.drctl(), 0);
  EXPECT_EQ(conreg.ssctl(), 0);
  EXPECT_EQ(conreg.smc(), 0);
  EXPECT_EQ(conreg.xch(), 0);
  EXPECT_EQ(conreg.mode(), ConReg::kModeMaster);
  EXPECT_EQ(conreg.en(), 1);

  EXPECT_EQ(enhanced_cntl.main_clock_always_on(), 0);
  EXPECT_EQ(enhanced_cntl.clk_cs_delay_enable(), 1);
  EXPECT_EQ(enhanced_cntl.cs_oen_enhance_enable(), 1);
  EXPECT_EQ(enhanced_cntl.clk_oen_enhance_enable(), 1);
  EXPECT_EQ(enhanced_cntl.mosi_oen_enhance_enable(), 1);
  EXPECT_EQ(enhanced_cntl.spi_clk_select(), 1);
  EXPECT_EQ(enhanced_cntl.enhance_clk_div(), 0xa5);
  EXPECT_EQ(enhanced_cntl.clk_cs_delay(), 0);

  EXPECT_EQ(testreg.dlyctl(), 0b00'11'00);
  EXPECT_EQ(testreg.clk_free_en(), 1);
}

class AmlSpiNormalClockModeInvalidDividerTest : public AmlSpiTest {
 public:
  amlogic_spi::amlspi_config_t GetAmlSpiMetadata() override {
    return amlogic_spi::amlspi_config_t{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0xa5,
        .use_enhanced_clock_mode = false,
    };
  }
};

TEST_F(AmlSpiNormalClockModeInvalidDividerTest, Test) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  EXPECT_TRUE(start_result.is_error());
}

class AmlSpiEnhancedClockModeInvalidDividerTest : public AmlSpiTest {
 public:
  amlogic_spi::amlspi_config_t GetAmlSpiMetadata() override {
    return amlogic_spi::amlspi_config_t{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0x1a5,
        .use_enhanced_clock_mode = true,
    };
  }
};

TEST_F(AmlSpiEnhancedClockModeInvalidDividerTest, Test) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  EXPECT_TRUE(start_result.is_error());
}

class AmlSpiBtiPaddrTest : public AmlSpiTest {
 public:
  static constexpr zx_paddr_t kDmaPaddrs[] = {0x1212'0000, 0xabab'000};

  virtual void SetUpBti(fake_pdev::FakePDevFidl::Config& config) override {
    zx::bti bti;
    ASSERT_OK(fake_bti_create_with_paddrs(kDmaPaddrs, std::size(kDmaPaddrs),
                                          bti.reset_and_get_address()));
    bti_local_ = bti.borrow();
    config.btis[0] = std::move(bti);
  }

 protected:
  zx::unowned_bti bti_local_;
};

TEST_F(AmlSpiBtiPaddrTest, ExchangeDma) {
  constexpr uint8_t kTxData[24] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd,
      0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba, 0xf1, 0x49, 0x00,
  };
  constexpr uint8_t kExpectedRxData[24] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
  };

  uint8_t reversed_tx_data[24];
  for (size_t i = 0; i < sizeof(kTxData); i += sizeof(uint64_t)) {
    uint64_t tmp;
    memcpy(&tmp, kTxData + i, sizeof(tmp));
    tmp = htobe64(tmp);
    memcpy(reversed_tx_data + i, &tmp, sizeof(tmp));
  }

  uint8_t reversed_expected_rx_data[24];
  for (size_t i = 0; i < sizeof(kExpectedRxData); i += sizeof(uint64_t)) {
    uint64_t tmp;
    memcpy(&tmp, kExpectedRxData + i, sizeof(tmp));
    tmp = htobe64(tmp);
    memcpy(reversed_expected_rx_data + i, &tmp, sizeof(tmp));
  }

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fake_bti_pinned_vmo_info_t dma_vmos[2] = {};
  size_t actual_vmos = 0;
  EXPECT_OK(
      fake_bti_get_pinned_vmos(bti_local_->get(), dma_vmos, std::size(dma_vmos), &actual_vmos));
  EXPECT_EQ(actual_vmos, std::size(dma_vmos));

  zx::vmo tx_dma_vmo(dma_vmos[0].vmo);
  zx::vmo rx_dma_vmo(dma_vmos[1].vmo);

  // Copy the reversed expected RX data to the RX VMO. The driver should copy this to the user
  // output buffer with the correct endianness.
  rx_dma_vmo.write(reversed_expected_rx_data, 0, sizeof(reversed_expected_rx_data));

  zx_paddr_t tx_paddr = 0;
  zx_paddr_t rx_paddr = 0;

  mmio()[AML_SPI_DRADDR].SetWriteCallback([&tx_paddr](uint64_t value) { tx_paddr = value; });
  mmio()[AML_SPI_DWADDR].SetWriteCallback([&rx_paddr](uint64_t value) { rx_paddr = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[24] = {};
  memcpy(buf, kTxData, sizeof(buf));

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(buf));
        EXPECT_BYTES_EQ(kExpectedRxData, result->value()->rxdata.data(), sizeof(buf));
        runtime_.Quit();
      });
  runtime_.Run();

  // Verify that the driver wrote the TX data to the TX VMO.
  EXPECT_OK(tx_dma_vmo.read(buf, 0, sizeof(buf)));
  EXPECT_BYTES_EQ(reversed_tx_data, buf, sizeof(buf));

  EXPECT_EQ(tx_paddr, kDmaPaddrs[0]);
  EXPECT_EQ(rx_paddr, kDmaPaddrs[1]);

  EXPECT_FALSE(ControllerReset());
}

class AmlSpiBtiEmptyTest : public AmlSpiTest {
 public:
  virtual void SetUpBti(fake_pdev::FakePDevFidl::Config& config) override {
    zx::bti bti;
    ASSERT_OK(fake_bti_create(bti.reset_and_get_address()));
    bti_local_ = bti.borrow();
    config.btis[0] = std::move(bti);
  }

 protected:
  zx::unowned_bti bti_local_;
};

TEST_F(AmlSpiBtiEmptyTest, ExchangeFallBackToPio) {
  constexpr uint8_t kTxData[15] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd, 0xd4, 0xcf, 0xa8,
  };
  constexpr uint8_t kExpectedRxData[15] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f,
  };

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fake_bti_pinned_vmo_info_t dma_vmos[2] = {};
  size_t actual_vmos = 0;
  EXPECT_OK(
      fake_bti_get_pinned_vmos(bti_local_->get(), dma_vmos, std::size(dma_vmos), &actual_vmos));
  EXPECT_EQ(actual_vmos, std::size(dma_vmos));

  zx_paddr_t tx_paddr = 0;
  zx_paddr_t rx_paddr = 0;

  mmio()[AML_SPI_DRADDR].SetWriteCallback([&tx_paddr](uint64_t value) { tx_paddr = value; });
  mmio()[AML_SPI_DWADDR].SetWriteCallback([&rx_paddr](uint64_t value) { rx_paddr = value; });

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea2b'8f8f; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[15] = {};
  memcpy(buf, kTxData, sizeof(buf));

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(buf));
        EXPECT_BYTES_EQ(kExpectedRxData, result->value()->rxdata.data(), sizeof(buf));
        runtime_.Quit();
      });
  runtime_.Run();

  EXPECT_EQ(tx_data, kTxData[14]);

  // Verify that DMA was not used.
  EXPECT_EQ(tx_paddr, 0);
  EXPECT_EQ(rx_paddr, 0);

  EXPECT_FALSE(ControllerReset());
}

class AmlSpiExchangeDmaClientReversesBufferTest : public AmlSpiBtiPaddrTest {
 public:
  amlogic_spi::amlspi_config_t GetAmlSpiMetadata() override {
    return amlogic_spi::amlspi_config_t{
        .bus_id = 0,
        .cs_count = 3,
        .cs = {5, 3, amlogic_spi::amlspi_config_t::kCsClientManaged},
        .clock_divider_register_value = 0,
        .use_enhanced_clock_mode = false,
        .client_reverses_dma_transfers = true,
    };
  }
};

TEST_F(AmlSpiExchangeDmaClientReversesBufferTest, Test) {
  constexpr uint8_t kTxData[24] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd,
      0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba, 0xf1, 0x49, 0x00,
  };
  constexpr uint8_t kExpectedRxData[24] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
  };

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fake_bti_pinned_vmo_info_t dma_vmos[2] = {};
  size_t actual_vmos = 0;
  EXPECT_OK(
      fake_bti_get_pinned_vmos(bti_local_->get(), dma_vmos, std::size(dma_vmos), &actual_vmos));
  EXPECT_EQ(actual_vmos, std::size(dma_vmos));

  zx::vmo tx_dma_vmo(dma_vmos[0].vmo);
  zx::vmo rx_dma_vmo(dma_vmos[1].vmo);

  rx_dma_vmo.write(kExpectedRxData, 0, sizeof(kExpectedRxData));

  zx_paddr_t tx_paddr = 0;
  zx_paddr_t rx_paddr = 0;

  mmio()[AML_SPI_DRADDR].SetWriteCallback([&tx_paddr](uint64_t value) { tx_paddr = value; });
  mmio()[AML_SPI_DWADDR].SetWriteCallback([&rx_paddr](uint64_t value) { rx_paddr = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[sizeof(kTxData)] = {};
  memcpy(buf, kTxData, sizeof(buf));

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(buf));
        EXPECT_BYTES_EQ(kExpectedRxData, result->value()->rxdata.data(), sizeof(buf));
        runtime_.Quit();
      });
  runtime_.Run();

  // Verify that the driver wrote the TX data to the TX VMO with the original byte order.
  EXPECT_OK(tx_dma_vmo.read(buf, 0, sizeof(buf)));
  EXPECT_BYTES_EQ(kTxData, buf, sizeof(buf));

  EXPECT_EQ(tx_paddr, kDmaPaddrs[0]);
  EXPECT_EQ(rx_paddr, kDmaPaddrs[1]);

  EXPECT_FALSE(ControllerReset());
}

class AmlSpiShutdownTest : public AmlSpiTest {
 public:
  // Override teardown so that the test case itself can call PrepareStop/Stop.
  void TearDown() override {}
};

TEST_F(AmlSpiShutdownTest, Shutdown) {
  // Must outlive AmlSpi device.
  bool dmareg_cleared = false;
  bool conreg_cleared = false;

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[16] = {};
  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        runtime_.Quit();
      });
  runtime_.Run();

  mmio()[AML_SPI_DMAREG].SetWriteCallback(
      [&dmareg_cleared](uint64_t value) { dmareg_cleared = value == 0; });

  mmio()[AML_SPI_CONREG].SetWriteCallback(
      [&conreg_cleared](uint64_t value) { conreg_cleared = value == 0; });

  zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
  EXPECT_OK(prepare_stop_result.status_value());
  EXPECT_TRUE(dut_.Stop().is_ok());

  EXPECT_TRUE(dmareg_cleared);
  EXPECT_TRUE(conreg_cleared);

  // All SPI devices have been released at this point, so no further calls can be made.

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

class AmlSpiNoResetFragmentTest : public AmlSpiTest {
 public:
  bool SetupResetRegister() override { return false; }
};

TEST_F(AmlSpiNoResetFragmentTest, ExchangeWithNoResetFragment) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  auto spiimpl_client = GetFidlClient();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  fdf::Arena arena('TEST');

  uint8_t buf[17] = {};
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 17))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 17);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  // Controller should not be reset because no reset fragment was provided.
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 16))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 16);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 3))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 3);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 6))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 6);
        EXPECT_FALSE(ControllerReset());
      });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, 8))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        EXPECT_EQ(result->value()->rxdata.count(), 8);
        EXPECT_FALSE(ControllerReset());
        runtime_.Quit();
      });
  runtime_.Run();

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

class AmlSpiNoIrqTest : public AmlSpiTest {
 public:
  virtual void SetUpInterrupt(fake_pdev::FakePDevFidl::Config& config) override {}
};

TEST_F(AmlSpiNoIrqTest, InterruptRequired) {
  // Bind should fail if no interrupt was provided.
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  EXPECT_TRUE(start_result.is_error());
}

}  // namespace spi

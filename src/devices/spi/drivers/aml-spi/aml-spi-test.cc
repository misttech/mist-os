// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-spi.h"

#include <endian.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
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

struct IncomingNamespace {
  explicit IncomingNamespace(const fdf::UnownedSynchronizedDispatcher& dispatcher)
      : registers(dispatcher->async_dispatcher()),
        node_server("root", dispatcher->async_dispatcher()),
        test_environment(dispatcher->get()) {}

  fake_pdev::FakePDevFidl pdev_server;
  mock_registers::MockRegisters registers;
  std::queue<std::pair<zx_status_t, uint8_t>> gpio_writes;
  fake_gpio::FakeGpio gpio;
  fdf_testing::TestNode node_server;
  fdf_testing::TestEnvironment test_environment;
  compat::DeviceServer compat;
};

class AmlSpiTest : public zxtest::Test {
 public:
  AmlSpiTest()
      : background_dispatcher_(runtime_.StartBackgroundDispatcher()),
        incoming_(background_dispatcher_->async_dispatcher(), std::in_place,
                  background_dispatcher_->borrow()),
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

    incoming_.SyncCall([this, config = std::move(config)](IncomingNamespace* incoming) mutable {
      zx::result start_args = incoming->node_server.CreateStartArgsAndServe();
      ASSERT_TRUE(start_args.is_ok());

      driver_outgoing_ = std::move(start_args->outgoing_directory_client);

      ASSERT_TRUE(
          incoming->test_environment.Initialize(std::move(start_args->incoming_directory_server))
              .is_ok());

      start_args_ = std::move(start_args->start_args);

      incoming->pdev_server.SetConfig(std::move(config));

      auto& directory = incoming->test_environment.incoming_directory();

      auto result = directory.AddService<fuchsia_hardware_platform_device::Service>(
          incoming->pdev_server.GetInstanceHandler(background_dispatcher_->async_dispatcher()),
          "pdev");
      ASSERT_TRUE(result.is_ok());

      const auto metadata = GetAmlSpiMetadata();
      EXPECT_OK(
          incoming->compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &metadata, sizeof(metadata)));
      incoming->compat.Init("pdev", {});
      EXPECT_OK(incoming->compat.Serve(background_dispatcher_->async_dispatcher(), &directory));

      result = directory.AddService<fuchsia_hardware_gpio::Service>(
          incoming->gpio.CreateInstanceHandler(), "gpio-cs-2");
      ASSERT_TRUE(result.is_ok());

      result = directory.AddService<fuchsia_hardware_gpio::Service>(
          incoming->gpio.CreateInstanceHandler(), "gpio-cs-3");
      ASSERT_TRUE(result.is_ok());

      result = directory.AddService<fuchsia_hardware_gpio::Service>(
          incoming->gpio.CreateInstanceHandler(), "gpio-cs-5");
      ASSERT_TRUE(result.is_ok());

      incoming->gpio.SetCurrentState(
          fake_gpio::State{.polarity = fuchsia_hardware_gpio::GpioPolarity::kHigh,
                           .sub_state = fake_gpio::WriteSubState{.value = 0}});
      incoming->gpio.SetWriteCallback([incoming](fake_gpio::FakeGpio& gpio) {
        if (incoming->gpio_writes.empty()) {
          EXPECT_FALSE(incoming->gpio_writes.empty());
          return ZX_ERR_INTERNAL;
        }
        auto [status, value] = incoming->gpio_writes.front();
        incoming->gpio_writes.pop();
        if (status != ZX_OK) {
          EXPECT_EQ(value, gpio.GetWriteValue());
        }
        return status;
      });
    });
    ASSERT_NO_FATAL_FAILURE();

    if (SetupResetRegister()) {
      incoming_.SyncCall([](IncomingNamespace* incoming) {
        auto result = incoming->test_environment.incoming_directory()
                          .AddService<fuchsia_hardware_registers::Service>(
                              incoming->registers.GetInstanceHandler(), "reset");
        ASSERT_TRUE(result.is_ok());
      });
      ASSERT_NO_FATAL_FAILURE();
    }

    incoming_.SyncCall([](IncomingNamespace* incoming) {
      incoming->registers.ExpectWrite<uint32_t>(0x1c, 1 << 1, 1 << 1);
    });
    ASSERT_NO_FATAL_FAILURE();
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(dut_.PrepareStop());
    EXPECT_OK(prepare_stop_result.status_value());
    EXPECT_TRUE(dut_.Stop().is_ok());
  }

  void ExpectGpioWrite(zx_status_t status, uint8_t value) {
    incoming_.SyncCall(
        [&](IncomingNamespace* incoming) { incoming->gpio_writes.emplace(status, value); });
  }

  void VerifyGpioAndClear() {
    incoming_.SyncCall([&](IncomingNamespace* incoming) {
      EXPECT_EQ(incoming->gpio_writes.size(), 0);
      incoming->gpio_writes = {};
    });
  }

  ddk_fake::FakeMmioRegRegion& mmio() { return dut_->mmio(); }
  bool ControllerReset() {
    zx_status_t status;
    incoming_.SyncCall([&status](IncomingNamespace* incoming) {
      status = incoming->registers.VerifyAll();
      if (status == ZX_OK) {
        // Always keep a single expectation in the queue, that way we can verify when the controller
        // is not reset.
        incoming->registers.ExpectWrite<uint32_t>(0x1c, 1 << 1, 1 << 1);
      }
    });

    return status == ZX_OK;
  }

 protected:
  ddk::SpiImplProtocolClient GetBanjoClient() {
    ddk::SpiImplProtocolClient client{};

    const auto path = std::string("svc/") +
                      component::MakeServiceMemberPath<fuchsia_driver_compat::Service::Device>(
                          component::kDefaultInstance);

    runtime_.PerformBlockingWork([&]() {
      auto client_end = component::ConnectAt<fuchsia_driver_compat::Device>(driver_outgoing_, path);
      ASSERT_TRUE(client_end.is_ok());

      fidl::WireSyncClient<fuchsia_driver_compat::Device> compat(*std::move(client_end));

      zx_info_handle_basic_t basic;
      ASSERT_OK(zx::process::self()->get_info(ZX_INFO_HANDLE_BASIC, &basic, sizeof(basic), nullptr,
                                              nullptr));

      auto banjo_client = compat->GetBanjoProtocol(ZX_PROTOCOL_SPI_IMPL, basic.koid);
      ASSERT_TRUE(banjo_client.ok());
      ASSERT_TRUE(banjo_client->is_ok());

      const spi_impl_protocol_t proto{
          .ops = reinterpret_cast<spi_impl_protocol_ops_t*>(banjo_client->value()->ops),
          .ctx = reinterpret_cast<void*>(banjo_client->value()->context),
      };
      client = ddk::SpiImplProtocolClient(&proto);
    });

    return client;
  }

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher background_dispatcher_;
  async_patterns::TestDispatcherBound<IncomingNamespace> incoming_;
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

  incoming_.SyncCall([](IncomingNamespace* incoming) {
    ASSERT_NE(incoming->node_server.children().find("aml-spi-0"),
              incoming->node_server.children().cend());
  });
}

TEST_F(AmlSpiTest, ChipSelectCount) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  EXPECT_EQ(spi0.GetChipSelectCount(), 3);
}

TEST_F(AmlSpiTest, Exchange) {
  constexpr uint8_t kTxData[] = {0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12};
  constexpr uint8_t kExpectedRxData[] = {0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t rxbuf[sizeof(kTxData)] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, kTxData, sizeof(kTxData), rxbuf, sizeof(rxbuf), &rx_actual));

  EXPECT_EQ(rx_actual, sizeof(rxbuf));
  EXPECT_BYTES_EQ(rxbuf, kExpectedRxData, rx_actual);
  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ExchangeCsManagedByClient) {
  constexpr uint8_t kTxData[] = {0x12, 0x12, 0x12, 0x12, 0x12, 0x12, 0x12};
  constexpr uint8_t kExpectedRxData[] = {0xab, 0xab, 0xab, 0xab, 0xab, 0xab, 0xab};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  uint8_t rxbuf[sizeof(kTxData)] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(2, kTxData, sizeof(kTxData), rxbuf, sizeof(rxbuf), &rx_actual));

  EXPECT_EQ(rx_actual, sizeof(rxbuf));
  EXPECT_BYTES_EQ(rxbuf, kExpectedRxData, rx_actual);
  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  // There should be no GPIO calls as the client manages CS for this device.
  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, RegisterVmo) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi1 = GetBanjoClient();
  ASSERT_TRUE(spi1.is_valid());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  const zx_koid_t test_vmo_koid = GetVmoKoid(test_vmo);

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));
  }

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_NOT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));
  }

  {
    zx::vmo vmo;
    EXPECT_OK(spi1.UnregisterVmo(0, 1, &vmo));
    EXPECT_EQ(test_vmo_koid, GetVmoKoid(vmo));
  }

  {
    zx::vmo vmo;
    EXPECT_NOT_OK(spi1.UnregisterVmo(0, 1, &vmo));
  }
}

TEST_F(AmlSpiTest, Transmit) {
  constexpr uint8_t kTxData[] = {0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi1 = GetBanjoClient();
  ASSERT_TRUE(spi1.is_valid());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 256, PAGE_SIZE - 256, SPI_VMO_RIGHT_READ));
  }

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(test_vmo.write(kTxData, 512, sizeof(kTxData)));

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  EXPECT_OK(spi1.TransmitVmo(0, 1, 256, sizeof(kTxData)));

  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ReceiveVmo) {
  constexpr uint8_t kExpectedRxData[] = {0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi1 = GetBanjoClient();
  ASSERT_TRUE(spi1.is_valid());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 256, PAGE_SIZE - 256,
                               SPI_VMO_RIGHT_READ | SPI_VMO_RIGHT_WRITE));
  }

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi1.ReceiveVmo(0, 1, 512, sizeof(kExpectedRxData)));

  uint8_t rx_buffer[sizeof(kExpectedRxData)];
  EXPECT_OK(test_vmo.read(rx_buffer, 768, sizeof(rx_buffer)));
  EXPECT_BYTES_EQ(rx_buffer, kExpectedRxData, sizeof(rx_buffer));

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ExchangeVmo) {
  constexpr uint8_t kTxData[] = {0xef, 0xef, 0xef, 0xef, 0xef, 0xef, 0xef};
  constexpr uint8_t kExpectedRxData[] = {0x78, 0x78, 0x78, 0x78, 0x78, 0x78, 0x78};

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi1 = GetBanjoClient();
  ASSERT_TRUE(spi1.is_valid());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 256, PAGE_SIZE - 256,
                               SPI_VMO_RIGHT_READ | SPI_VMO_RIGHT_WRITE));
  }

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return kExpectedRxData[0]; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(test_vmo.write(kTxData, 512, sizeof(kTxData)));

  EXPECT_OK(spi1.ExchangeVmo(0, 1, 256, 1, 512, sizeof(kTxData)));

  uint8_t rx_buffer[sizeof(kExpectedRxData)];
  EXPECT_OK(test_vmo.read(rx_buffer, 768, sizeof(rx_buffer)));
  EXPECT_BYTES_EQ(rx_buffer, kExpectedRxData, sizeof(rx_buffer));

  EXPECT_EQ(tx_data, kTxData[0]);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, TransfersOutOfRange) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(spi0.RegisterVmo(1, 1, std::move(vmo), PAGE_SIZE - 4, 4,
                               SPI_VMO_RIGHT_READ | SPI_VMO_RIGHT_WRITE));
  }

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.ExchangeVmo(1, 1, 0, 1, 2, 2));
  EXPECT_NOT_OK(spi0.ExchangeVmo(1, 1, 0, 1, 3, 2));
  EXPECT_NOT_OK(spi0.ExchangeVmo(1, 1, 3, 1, 0, 2));
  EXPECT_NOT_OK(spi0.ExchangeVmo(1, 1, 0, 1, 2, 3));

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.TransmitVmo(1, 1, 0, 4));
  EXPECT_NOT_OK(spi0.TransmitVmo(1, 1, 0, 5));
  EXPECT_NOT_OK(spi0.TransmitVmo(1, 1, 3, 2));
  EXPECT_NOT_OK(spi0.TransmitVmo(1, 1, 4, 1));
  EXPECT_NOT_OK(spi0.TransmitVmo(1, 1, 5, 1));

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);
  EXPECT_OK(spi0.ReceiveVmo(1, 1, 0, 4));

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);
  EXPECT_OK(spi0.ReceiveVmo(1, 1, 3, 1));

  EXPECT_NOT_OK(spi0.ReceiveVmo(1, 1, 3, 2));
  EXPECT_NOT_OK(spi0.ReceiveVmo(1, 1, 4, 1));
  EXPECT_NOT_OK(spi0.ReceiveVmo(1, 1, 5, 1));

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, VmoBadRights) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi1 = GetBanjoClient();
  ASSERT_TRUE(spi1.is_valid());

  zx::vmo test_vmo;
  EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &test_vmo));

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 0, 256, SPI_VMO_RIGHT_READ));
  }

  {
    zx::vmo vmo;
    EXPECT_OK(test_vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo));
    EXPECT_OK(
        spi1.RegisterVmo(0, 2, std::move(vmo), 0, 256, SPI_VMO_RIGHT_READ | SPI_VMO_RIGHT_WRITE));
  }

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi1.ExchangeVmo(0, 1, 0, 2, 128, 128));
  EXPECT_EQ(spi1.ExchangeVmo(0, 2, 0, 1, 128, 128), ZX_ERR_ACCESS_DENIED);
  EXPECT_EQ(spi1.ExchangeVmo(0, 1, 0, 1, 128, 128), ZX_ERR_ACCESS_DENIED);
  EXPECT_EQ(spi1.ReceiveVmo(0, 1, 0, 128), ZX_ERR_ACCESS_DENIED);

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, Exchange64BitWords) {
  constexpr uint8_t kTxData[] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd,
      0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba, 0xf1, 0x49, 0x00,
  };
  constexpr uint8_t kExpectedRxData[] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
  };

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  // First (and only) word of kExpectedRxData with bytes swapped.
  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea2b'8f8f; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t rxbuf[sizeof(kTxData)] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, kTxData, sizeof(kTxData), rxbuf, sizeof(rxbuf), &rx_actual));

  EXPECT_EQ(rx_actual, sizeof(rxbuf));
  EXPECT_BYTES_EQ(rxbuf, kExpectedRxData, rx_actual);
  // Last word of kTxData with bytes swapped.
  EXPECT_EQ(tx_data, 0xbaf1'4900);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, Exchange64Then8BitWords) {
  constexpr uint8_t kTxData[] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb,
      0xbd, 0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba,
  };
  constexpr uint8_t kExpectedRxData[] = {
      0x00, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00, 0xea, 0x00, 0x00, 0x00,
      0xea, 0x00, 0x00, 0x00, 0xea, 0xea, 0xea, 0xea, 0xea, 0xea,
  };

  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea; });

  uint64_t tx_data = 0;
  mmio()[AML_SPI_TXDATA].SetWriteCallback([&tx_data](uint64_t value) { tx_data = value; });

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t rxbuf[sizeof(kTxData)] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, kTxData, sizeof(kTxData), rxbuf, sizeof(rxbuf), &rx_actual));

  EXPECT_EQ(rx_actual, sizeof(rxbuf));
  EXPECT_BYTES_EQ(rxbuf, kExpectedRxData, rx_actual);
  EXPECT_EQ(tx_data, 0xba);

  EXPECT_FALSE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ExchangeResetsController) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[17] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, buf, 17, buf, 17, &rx_actual));
  EXPECT_EQ(rx_actual, 17);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  // Controller should be reset because a 64-bit transfer was preceded by a transfer of an odd
  // number of bytes.
  EXPECT_OK(spi0.Exchange(0, buf, 16, buf, 16, &rx_actual));
  EXPECT_EQ(rx_actual, 16);
  EXPECT_TRUE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.Exchange(0, buf, 3, buf, 3, &rx_actual));
  EXPECT_EQ(rx_actual, 3);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.Exchange(0, buf, 6, buf, 6, &rx_actual));
  EXPECT_EQ(rx_actual, 6);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.Exchange(0, buf, 8, buf, 8, &rx_actual));
  EXPECT_EQ(rx_actual, 8);
  EXPECT_TRUE(ControllerReset());

  ASSERT_NO_FATAL_FAILURE(VerifyGpioAndClear());
}

TEST_F(AmlSpiTest, ReleaseVmos) {
  zx::result start_result = runtime_.RunToCompletion(dut_.Start(std::move(start_args_)));
  ASSERT_TRUE(start_result.is_ok());

  ddk::SpiImplProtocolClient spi1 = GetBanjoClient();
  ASSERT_TRUE(spi1.is_valid());

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 2, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));
  }

  {
    zx::vmo vmo;
    EXPECT_OK(spi1.UnregisterVmo(0, 2, &vmo));
  }

  // Release VMO 1 and make sure that a subsequent call to unregister it fails.
  spi1.ReleaseRegisteredVmos(0);

  {
    zx::vmo vmo;
    EXPECT_NOT_OK(spi1.UnregisterVmo(0, 1, &vmo));
  }

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 2, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));
  }

  // Release both VMOs and make sure that they can be registered again.
  spi1.ReleaseRegisteredVmos(0);

  {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 1, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));

    EXPECT_OK(zx::vmo::create(PAGE_SIZE, 0, &vmo));
    EXPECT_OK(spi1.RegisterVmo(0, 2, std::move(vmo), 0, PAGE_SIZE, SPI_VMO_RIGHT_READ));
  }
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

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

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

  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, buf, sizeof(buf), buf, sizeof(buf), &rx_actual));
  EXPECT_EQ(rx_actual, sizeof(buf));
  EXPECT_BYTES_EQ(kExpectedRxData, buf, sizeof(buf));

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

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

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

  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, buf, sizeof(buf), buf, sizeof(buf), &rx_actual));
  EXPECT_EQ(rx_actual, sizeof(buf));
  EXPECT_BYTES_EQ(kExpectedRxData, buf, sizeof(buf));
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

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

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

  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, buf, sizeof(buf), buf, sizeof(buf), &rx_actual));
  EXPECT_EQ(rx_actual, sizeof(buf));
  EXPECT_BYTES_EQ(kExpectedRxData, buf, sizeof(buf));

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

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[16] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, buf, sizeof(buf), buf, sizeof(buf), &rx_actual));

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

  ddk::SpiImplProtocolClient spi0 = GetBanjoClient();
  ASSERT_TRUE(spi0.is_valid());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  uint8_t buf[17] = {};
  size_t rx_actual;
  EXPECT_OK(spi0.Exchange(0, buf, 17, buf, 17, &rx_actual));
  EXPECT_EQ(rx_actual, 17);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  // Controller should not be reset because no reset fragment was provided.
  EXPECT_OK(spi0.Exchange(0, buf, 16, buf, 16, &rx_actual));
  EXPECT_EQ(rx_actual, 16);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.Exchange(0, buf, 3, buf, 3, &rx_actual));
  EXPECT_EQ(rx_actual, 3);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.Exchange(0, buf, 6, buf, 6, &rx_actual));
  EXPECT_EQ(rx_actual, 6);
  EXPECT_FALSE(ControllerReset());

  ExpectGpioWrite(ZX_OK, 0);
  ExpectGpioWrite(ZX_OK, 1);

  EXPECT_OK(spi0.Exchange(0, buf, 8, buf, 8, &rx_actual));
  EXPECT_EQ(rx_actual, 8);
  EXPECT_FALSE(ControllerReset());

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

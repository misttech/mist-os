// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/spi/drivers/aml-spi/tests/aml-spi-test-env.h"

namespace spi {

namespace {
bool IsBytesEqual(const uint8_t* expected, const uint8_t* actual, size_t len) {
  return memcmp(expected, actual, len) == 0;
}
}  // namespace

class AmlSpiBtiPaddrEnvironment : public BaseTestEnvironment {
 public:
  static constexpr zx_paddr_t kDmaPaddrs[] = {0x1212'0000, 0xabab'000};

  virtual void SetUpBti() override {
    zx::bti bti;
    ASSERT_OK(fake_bti_create_with_paddrs(kDmaPaddrs, std::size(kDmaPaddrs),
                                          bti.reset_and_get_address()));
    bti_local_ = bti.borrow();
    pdev_server_.set_bti(std::move(bti));
  }

  zx::unowned_bti& GetBtiLocal() { return bti_local_; }

 private:
  zx::unowned_bti bti_local_;
};

class AmlSpiBtiPaddrFixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiBtiPaddrEnvironment;
};

class AmlSpiBtiPaddrTest : public fdf_testing::DriverTestFixture<AmlSpiBtiPaddrFixtureConfig> {};

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

  auto spiimpl_client = Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fake_bti_pinned_vmo_info_t dma_vmos[2] = {};
  size_t actual_vmos = 0;
  RunInEnvironmentTypeContext([&dma_vmos, &actual_vmos](AmlSpiBtiPaddrEnvironment& env) {
    EXPECT_OK(fake_bti_get_pinned_vmos(env.GetBtiLocal()->get(), dma_vmos, std::size(dma_vmos),
                                       &actual_vmos));
    EXPECT_EQ(actual_vmos, std::size(dma_vmos));
  });

  zx::vmo tx_dma_vmo(dma_vmos[0].vmo);
  zx::vmo rx_dma_vmo(dma_vmos[1].vmo);

  // Copy the reversed expected RX data to the RX VMO. The driver should copy this to the user
  // output buffer with the correct endianness.
  rx_dma_vmo.write(reversed_expected_rx_data, 0, sizeof(reversed_expected_rx_data));

  zx_paddr_t tx_paddr = 0;
  zx_paddr_t rx_paddr = 0;

  driver()->mmio()[AML_SPI_DRADDR].SetWriteCallback(
      [&tx_paddr](uint64_t value) { tx_paddr = value; });
  driver()->mmio()[AML_SPI_DWADDR].SetWriteCallback(
      [&rx_paddr](uint64_t value) { rx_paddr = value; });

  RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    env.ExpectGpioWrite(ZX_OK, 0);
    env.ExpectGpioWrite(ZX_OK, 1);
  });

  uint8_t buf[24] = {};
  memcpy(buf, kTxData, sizeof(buf));

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(buf));
        EXPECT_TRUE(IsBytesEqual(kExpectedRxData, result->value()->rxdata.data(), sizeof(buf)));
        runtime().Quit();
      });
  runtime().Run();

  // Verify that the driver wrote the TX data to the TX VMO.
  EXPECT_OK(tx_dma_vmo.read(buf, 0, sizeof(buf)));
  EXPECT_TRUE(IsBytesEqual(reversed_tx_data, buf, sizeof(buf)));

  EXPECT_EQ(tx_paddr, AmlSpiBtiPaddrEnvironment::kDmaPaddrs[0]);
  EXPECT_EQ(rx_paddr, AmlSpiBtiPaddrEnvironment::kDmaPaddrs[1]);

  RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
}

class AmlSpiBtiEmptyEnvironment : public BaseTestEnvironment {
 public:
  static constexpr zx_paddr_t kDmaPaddrs[] = {0x1212'0000, 0xabab'000};

  virtual void SetUpBti() override {
    zx::bti bti;
    ASSERT_OK(fake_bti_create(bti.reset_and_get_address()));
    bti_local_ = bti.borrow();
    pdev_server_.set_bti(std::move(bti));
  }

  zx::unowned_bti& GetBtiLocal() { return bti_local_; }

 private:
  zx::unowned_bti bti_local_;
};

class AmlSpiBtiEmptyFixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiBtiPaddrEnvironment;
};

class AmlSpiBtiEmptyTest : public fdf_testing::DriverTestFixture<AmlSpiBtiEmptyFixtureConfig> {};

TEST_F(AmlSpiBtiEmptyTest, ExchangeFallBackToPio) {
  constexpr uint8_t kTxData[15] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd, 0xd4, 0xcf, 0xa8,
  };
  constexpr uint8_t kExpectedRxData[15] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f, 0x8f,
  };

  auto spiimpl_client = Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fake_bti_pinned_vmo_info_t dma_vmos[2] = {};
  size_t actual_vmos = 0;
  RunInEnvironmentTypeContext([&dma_vmos, &actual_vmos](AmlSpiBtiPaddrEnvironment& env) {
    EXPECT_OK(fake_bti_get_pinned_vmos(env.GetBtiLocal()->get(), dma_vmos, std::size(dma_vmos),
                                       &actual_vmos));
    EXPECT_EQ(actual_vmos, std::size(dma_vmos));
  });

  zx_paddr_t tx_paddr = 0;
  zx_paddr_t rx_paddr = 0;

  driver()->mmio()[AML_SPI_DRADDR].SetWriteCallback(
      [&tx_paddr](uint64_t value) { tx_paddr = value; });
  driver()->mmio()[AML_SPI_DWADDR].SetWriteCallback(
      [&rx_paddr](uint64_t value) { rx_paddr = value; });

  driver()->mmio()[AML_SPI_RXDATA].SetReadCallback([]() { return 0xea2b'8f8f; });

  uint64_t tx_data = 0;
  driver()->mmio()[AML_SPI_TXDATA].SetWriteCallback(
      [&tx_data](uint64_t value) { tx_data = value; });

  RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    env.ExpectGpioWrite(ZX_OK, 0);
    env.ExpectGpioWrite(ZX_OK, 1);
  });

  uint8_t buf[15] = {};
  memcpy(buf, kTxData, sizeof(buf));

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(buf));
        EXPECT_TRUE(IsBytesEqual(kExpectedRxData, result->value()->rxdata.data(), sizeof(buf)));
        runtime().Quit();
      });
  runtime().Run();

  EXPECT_EQ(tx_data, kTxData[14]);

  // Verify that DMA was not used.
  EXPECT_EQ(tx_paddr, 0u);
  EXPECT_EQ(rx_paddr, 0u);

  RunInEnvironmentTypeContext(
      [](BaseTestEnvironment& env) { EXPECT_FALSE(env.ControllerReset()); });
}

class AmlSpiExchangeDmaClientReversesBufferEnvironment : public AmlSpiBtiPaddrEnvironment {
 public:
  void SetMetadata(compat::DeviceServer& compat) override {
    constexpr amlogic_spi::amlspi_config_t kSpiConfig{
        .bus_id = 0,
        .cs_count = 3,
        .cs = {5, 3, amlogic_spi::amlspi_config_t::kCsClientManaged},
        .clock_divider_register_value = 0,
        .use_enhanced_clock_mode = false,
        .client_reverses_dma_transfers = true,
    };

    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));
  }
};

class AmlSpiExchangeDmaClientReversesBufferConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiExchangeDmaClientReversesBufferEnvironment;
};

class AmlSpiExchangeDmaClientReversesBufferTest
    : public fdf_testing::DriverTestFixture<AmlSpiExchangeDmaClientReversesBufferConfig> {};

TEST_F(AmlSpiExchangeDmaClientReversesBufferTest, Test) {
  constexpr uint8_t kTxData[24] = {
      0x3c, 0xa7, 0x5f, 0xc8, 0x4b, 0x0b, 0xdf, 0xef, 0xb9, 0xa0, 0xcb, 0xbd,
      0xd4, 0xcf, 0xa8, 0xbf, 0x85, 0xf2, 0x6a, 0xe3, 0xba, 0xf1, 0x49, 0x00,
  };
  constexpr uint8_t kExpectedRxData[24] = {
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
      0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f, 0xea, 0x2b, 0x8f, 0x8f,
  };

  auto spiimpl_client = Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  fake_bti_pinned_vmo_info_t dma_vmos[2] = {};
  size_t actual_vmos = 0;
  RunInEnvironmentTypeContext([&dma_vmos, &actual_vmos](AmlSpiBtiPaddrEnvironment& env) {
    EXPECT_OK(fake_bti_get_pinned_vmos(env.GetBtiLocal()->get(), dma_vmos, std::size(dma_vmos),
                                       &actual_vmos));
    EXPECT_EQ(actual_vmos, std::size(dma_vmos));
  });

  zx::vmo tx_dma_vmo(dma_vmos[0].vmo);
  zx::vmo rx_dma_vmo(dma_vmos[1].vmo);

  rx_dma_vmo.write(kExpectedRxData, 0, sizeof(kExpectedRxData));

  zx_paddr_t tx_paddr = 0;
  zx_paddr_t rx_paddr = 0;

  driver()->mmio()[AML_SPI_DRADDR].SetWriteCallback(
      [&tx_paddr](uint64_t value) { tx_paddr = value; });
  driver()->mmio()[AML_SPI_DWADDR].SetWriteCallback(
      [&rx_paddr](uint64_t value) { rx_paddr = value; });

  RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    env.ExpectGpioWrite(ZX_OK, 0);
    env.ExpectGpioWrite(ZX_OK, 1);
  });

  uint8_t buf[sizeof(kTxData)] = {};
  memcpy(buf, kTxData, sizeof(buf));

  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok());
        ASSERT_TRUE(result->is_ok());
        ASSERT_EQ(result->value()->rxdata.count(), sizeof(buf));
        EXPECT_TRUE(IsBytesEqual(kExpectedRxData, result->value()->rxdata.data(), sizeof(buf)));
        runtime().Quit();
      });
  runtime().Run();

  // Verify that the driver wrote the TX data to the TX VMO with the original byte order.
  EXPECT_OK(tx_dma_vmo.read(buf, 0, sizeof(buf)));
  EXPECT_TRUE(IsBytesEqual(kTxData, buf, sizeof(buf)));

  EXPECT_EQ(tx_paddr, AmlSpiBtiPaddrEnvironment::kDmaPaddrs[0]);
  EXPECT_EQ(rx_paddr, AmlSpiBtiPaddrEnvironment::kDmaPaddrs[1]);

  RunInEnvironmentTypeContext([](AmlSpiExchangeDmaClientReversesBufferEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
  });
}

}  // namespace spi

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/spi/drivers/aml-spi/tests/aml-spi-test-env.h"

namespace spi {

class AmlSpiShutdownConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = false;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = BaseTestEnvironment;
};

class AmlSpiShutdownTest : public fdf_testing::DriverTestFixture<AmlSpiShutdownConfig> {};

TEST_F(AmlSpiShutdownTest, Shutdown) {
  // Must outlive AmlSpi device.
  bool dmareg_cleared = false;
  bool conreg_cleared = false;

  auto spiimpl_client = Connect<fuchsia_hardware_spiimpl::Service::Device>();
  ASSERT_TRUE(spiimpl_client.is_ok());

  fdf::WireClient<fuchsia_hardware_spiimpl::SpiImpl> spiimpl(*std::move(spiimpl_client),
                                                             fdf::Dispatcher::GetCurrent()->get());

  RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    env.ExpectGpioWrite(ZX_OK, 0);
    env.ExpectGpioWrite(ZX_OK, 1);
  });

  uint8_t buf[16] = {};
  fdf::Arena arena('TEST');
  spiimpl.buffer(arena)
      ->ExchangeVector(0, fidl::VectorView<uint8_t>::FromExternal(buf, sizeof(buf)))
      .Then([&](auto& result) {
        ASSERT_TRUE(result.ok() && result->is_ok());
        runtime().Quit();
      });
  runtime().Run();

  driver()->mmio()[AML_SPI_DMAREG].SetWriteCallback(
      [&dmareg_cleared](uint64_t value) { dmareg_cleared = value == 0; });

  driver()->mmio()[AML_SPI_CONREG].SetWriteCallback(
      [&conreg_cleared](uint64_t value) { conreg_cleared = value == 0; });

  EXPECT_TRUE(StopDriver().is_ok());

  // All SPI devices have been released at this point, so no further calls can be made.
  RunInEnvironmentTypeContext([](BaseTestEnvironment& env) {
    EXPECT_FALSE(env.ControllerReset());
    ASSERT_NO_FATAL_FAILURE(env.VerifyGpioAndClear());
  });

  ShutdownDispatchersAndDestroyDriver();

  EXPECT_TRUE(dmareg_cleared);
  EXPECT_TRUE(conreg_cleared);
}

}  // namespace spi

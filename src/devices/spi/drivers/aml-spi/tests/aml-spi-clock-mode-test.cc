// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/spi/drivers/aml-spi/tests/aml-spi-test-env.h"

namespace spi {

class AmlSpiNormalClockModeTestEnvironment : public BaseTestEnvironment {
  void SetMetadata(compat::DeviceServer& compat) override {
    constexpr amlogic_spi::amlspi_config_t kSpiConfig{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0x5,
        .use_enhanced_clock_mode = false,
    };

    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));
  }
};

class AmlSpiNormalClockModeFixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiNormalClockModeTestEnvironment;
};

class AmlSpiNormalClockModeTest
    : public fdf_testing::DriverTestFixture<AmlSpiNormalClockModeFixtureConfig> {};

TEST_F(AmlSpiNormalClockModeTest, Test) {
  auto conreg = ConReg::Get().FromValue(driver()->conreg());
  auto enhanced_cntl = EnhanceCntl::Get().FromValue(driver()->enhance_cntl());
  auto testreg = TestReg::Get().FromValue(driver()->testreg());

  EXPECT_EQ(conreg.data_rate(), 0x5u);
  EXPECT_EQ(conreg.drctl(), 0u);
  EXPECT_EQ(conreg.ssctl(), 0u);
  EXPECT_EQ(conreg.smc(), 0u);
  EXPECT_EQ(conreg.xch(), 0u);
  EXPECT_EQ(conreg.mode(), ConReg::kModeMaster);
  EXPECT_EQ(conreg.en(), 1u);

  EXPECT_EQ(enhanced_cntl.reg_value(), 0u);

  EXPECT_EQ(testreg.dlyctl(), 0x15u);
  EXPECT_EQ(testreg.clk_free_en(), 1u);
}

class AmlSpiEnhancedClockModeEnvironment : public BaseTestEnvironment {
  void SetMetadata(compat::DeviceServer& compat) override {
    constexpr amlogic_spi::amlspi_config_t kSpiConfig{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0xa5,
        .use_enhanced_clock_mode = true,
        .delay_control = 0b00'11'00,
    };

    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));
  }
};

class AmlSpiEnhancedClockModeFixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = true;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiEnhancedClockModeEnvironment;
};

class AmlSpiEnhancedClockModeTest
    : public fdf_testing::DriverTestFixture<AmlSpiEnhancedClockModeFixtureConfig> {};

TEST_F(AmlSpiEnhancedClockModeTest, Test) {
  auto conreg = ConReg::Get().FromValue(driver()->conreg());
  auto enhanced_cntl = EnhanceCntl::Get().FromValue(driver()->enhance_cntl());
  auto testreg = TestReg::Get().FromValue(driver()->testreg());

  EXPECT_EQ(conreg.data_rate(), 0u);
  EXPECT_EQ(conreg.drctl(), 0u);
  EXPECT_EQ(conreg.ssctl(), 0u);
  EXPECT_EQ(conreg.smc(), 0u);
  EXPECT_EQ(conreg.xch(), 0u);
  EXPECT_EQ(conreg.mode(), ConReg::kModeMaster);
  EXPECT_EQ(conreg.en(), 1u);

  EXPECT_EQ(enhanced_cntl.main_clock_always_on(), 0u);
  EXPECT_EQ(enhanced_cntl.clk_cs_delay_enable(), 1u);
  EXPECT_EQ(enhanced_cntl.cs_oen_enhance_enable(), 1u);
  EXPECT_EQ(enhanced_cntl.clk_oen_enhance_enable(), 1u);
  EXPECT_EQ(enhanced_cntl.mosi_oen_enhance_enable(), 1u);
  EXPECT_EQ(enhanced_cntl.spi_clk_select(), 1u);
  EXPECT_EQ(enhanced_cntl.enhance_clk_div(), 0xa5u);
  EXPECT_EQ(enhanced_cntl.clk_cs_delay(), 0u);

  EXPECT_EQ(testreg.dlyctl(), 0b00'11'00u);
  EXPECT_EQ(testreg.clk_free_en(), 1u);
}

class AmlSpiNormalClockModeInvalidDividerEnvironment : public BaseTestEnvironment {
  void SetMetadata(compat::DeviceServer& compat) override {
    constexpr amlogic_spi::amlspi_config_t kSpiConfig{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0xa5,
        .use_enhanced_clock_mode = false,
    };

    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));
  }
};

class AmlSpiNormalClockModeInvalidDividerFixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = false;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiNormalClockModeInvalidDividerEnvironment;
};

class AmlSpiNormalClockModeInvalidDividerTest
    : public fdf_testing::DriverTestFixture<AmlSpiNormalClockModeInvalidDividerFixtureConfig> {};

TEST_F(AmlSpiNormalClockModeInvalidDividerTest, Test) { EXPECT_TRUE(StartDriver().is_error()); }

class AmlSpiEnhancedClockModeInvalidDividerEnvironment : public BaseTestEnvironment {
 public:
  void SetMetadata(compat::DeviceServer& compat) override {
    constexpr amlogic_spi::amlspi_config_t kSpiConfig{
        .bus_id = 0,
        .cs_count = 2,
        .cs = {5, 3},
        .clock_divider_register_value = 0x1a5,
        .use_enhanced_clock_mode = true,
    };

    EXPECT_OK(compat.AddMetadata(DEVICE_METADATA_AMLSPI_CONFIG, &kSpiConfig, sizeof(kSpiConfig)));
  }
};

class AmlSpiEnhancedClockModeInvalidDividerFixtureConfig final {
 public:
  static constexpr bool kDriverOnForeground = true;
  static constexpr bool kAutoStartDriver = false;
  static constexpr bool kAutoStopDriver = true;

  using DriverType = TestAmlSpiDriver;
  using EnvironmentType = AmlSpiEnhancedClockModeInvalidDividerEnvironment;
};

class AmlSpiEnhancedClockModeInvalidDividerTest
    : public fdf_testing::DriverTestFixture<AmlSpiEnhancedClockModeInvalidDividerFixtureConfig> {};

TEST_F(AmlSpiEnhancedClockModeInvalidDividerTest, Test) { EXPECT_TRUE(StartDriver().is_error()); }

}  // namespace spi

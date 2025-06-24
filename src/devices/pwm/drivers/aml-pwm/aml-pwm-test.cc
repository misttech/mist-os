// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-pwm.h"

#include <lib/driver/fake-platform-device/cpp/fake-pdev.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <vector>

#include <gtest/gtest.h>
#include <mock-mmio-reg/mock-mmio-reg.h>
#include <soc/aml-common/aml-pwm-regs.h>

#include "src/lib/testing/predicates/status.h"

namespace pwm {

class AmlPwmDriverTestEnvironment : public fdf_testing::Environment {
 public:
  void Init() {
    static constexpr size_t kRegSize = 0x00001000 / sizeof(uint32_t);  // in 32 bits chunks.
    static constexpr size_t kMmioCount = 5;

    // Protect channel 3 for protect tests
    static const fuchsia_hardware_pwm::PwmChannelsMetadata kMetadata{
        {{{{{.id = 0}},
           {{.id = 1}},
           {{.id = 2}},
           {{.id = 3, .skip_init = true}},
           {{.id = 4}},
           {{.id = 5}},
           {{.id = 6}},
           {{.id = 7}},
           {{.id = 8}},
           {{.id = 9}}}}}};

    device_server_.Initialize(component::kDefaultInstance);

    std::map<uint32_t, fdf_fake::Mmio> mmios;
    for (size_t i = 0; i < kMmioCount; ++i) {
      auto& mmio = *mmios_.emplace_back(
          std::make_unique<ddk_mock::MockMmioRegRegion>(sizeof(uint32_t), kRegSize));
      // Even numbered channel SetMode calls.
      mmio[2lu * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFDFFFFFA);

      // Odd numbered channel SetMode calls. However, initialization will be skipped for channel 3.
      if (i != 1) {
        mmio[2lu * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFEFFFFF5);
      }

      mmios.insert({i, mmio.GetMmioBuffer()});
    }

    pdev_.SetConfig({.mmios = std::move(mmios), .device_info{{.mmio_count = kMmioCount}}});
    pdev_.AddFidlMetadata(fuchsia_hardware_pwm::PwmChannelsMetadata::kSerializableName, kMetadata);
  }

  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto* dispatcher = fdf::Dispatcher::GetCurrent()->async_dispatcher();

    zx_status_t status = device_server_.Serve(dispatcher, &to_driver_vfs);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    {
      zx::result result = to_driver_vfs.AddService<fuchsia_hardware_platform_device::Service>(
          pdev_.GetInstanceHandler(dispatcher));
      if (result.is_error()) {
        return result.take_error();
      }
    }

    return zx::ok();
  }

  std::span<std::unique_ptr<ddk_mock::MockMmioRegRegion>> mmios() { return mmios_; }

 private:
  std::vector<std::unique_ptr<ddk_mock::MockMmioRegRegion>> mmios_;
  compat::DeviceServer device_server_;
  fdf_fake::FakePDev pdev_;
};

class FixtureConfig final {
 public:
  using DriverType = AmlPwmDriver;
  using EnvironmentType = AmlPwmDriverTestEnvironment;
};

class AmlPwmDriverTest : public ::testing::Test {
 public:
  void SetUp() override {
    driver_test_.RunInEnvironmentTypeContext([](auto& env) { env.Init(); });
    ASSERT_OK(driver_test_.StartDriver());
  }

  void TearDown() override {
    ASSERT_OK(driver_test_.StopDriver());
    WithMmios([](auto mmios) {
      for (auto& mmio : mmios) {
        mmio->VerifyAll();
      }
    });
  }

 protected:
  void WithMmios(
      fit::callback<void(std::span<std::unique_ptr<ddk_mock::MockMmioRegRegion>>)> callback) {
    driver_test_.RunInEnvironmentTypeContext(
        [callback = std::move(callback)](auto& env) mutable { callback(env.mmios()); });
  }

  AmlPwmDriver& driver() { return *driver_test_.driver(); }

 private:
  fdf_testing::ForegroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(AmlPwmDriverTest, ProtectTest) {
  mode_config mode_cfg{
      .mode = static_cast<Mode>(100),
      .regular = {},
  };
  pwm_config cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&mode_cfg),
      .mode_config_size = sizeof(mode_cfg),
  };
  EXPECT_NE(driver().PwmImplSetConfig(3, &cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, GetConfigTest) {
  mode_config mode_cfg{
      .mode = static_cast<Mode>(100),
      .regular = {},
  };
  pwm_config cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&mode_cfg),
      .mode_config_size = sizeof(mode_cfg),
  };
  EXPECT_OK(driver().PwmImplGetConfig(0, &cfg));

  cfg.mode_config_buffer = nullptr;
  EXPECT_NE(driver().PwmImplGetConfig(0, &cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidNullConfig) {
  // config is null
  EXPECT_NE(driver().PwmImplSetConfig(0, nullptr), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidNoModeBuffer) {
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      // config has no mode buffer
      .mode_config_buffer = nullptr,
      .mode_config_size = 0,
  };
  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidModeConfigSizeIncorrect) {
  mode_config fail_mode{
      .mode = Mode::kOn,
      .regular = {},
  };
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail_mode),
      // mode_config_size incorrect
      .mode_config_size = 10,
  };

  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidTwoTimerTimer2InvalidDutyCycle) {
  mode_config fail_mode{
      .mode = Mode::kTwoTimer,
      .two_timer = {},
  };
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail_mode),
      .mode_config_size = sizeof(fail_mode),
  };

  // Invalid duty cycle for timer 2.
  fail_mode.two_timer.duty_cycle2 = -10.0;
  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);

  fail_mode.two_timer.duty_cycle2 = 120.0;
  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidTimer1InvalidDutyCycle) {
  mode_config fail_mode{
      .mode = Mode::kOn,
      .regular = {},
  };
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail_mode),
      .mode_config_size = sizeof(fail_mode),
  };

  // Invalid duty cycle for timer 1.
  fail_cfg.duty_cycle = -10.0;
  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);

  fail_cfg.duty_cycle = 120.0;
  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidTimer1InvalidMode) {
  mode_config fail_mode{
      // Invalid mode
      .mode = static_cast<Mode>(100),
      .regular = {},
  };
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail_mode),
      .mode_config_size = sizeof(fail_mode),
  };

  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidPwmId) {
  for (Mode mode : {Mode::kOn, Mode::kOff, Mode::kTwoTimer, Mode::kDeltaSigma}) {
    mode_config fail{
        .mode = mode,
    };
    pwm_config fail_cfg{
        .polarity = false,
        .period_ns = 1250,
        .duty_cycle = 100.0,
        .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail),
        .mode_config_size = sizeof(fail),
    };
    // Incorrect pwm ID.
    EXPECT_NE(driver().PwmImplSetConfig(10, &fail_cfg), ZX_OK);
  }
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidTimer1PeriodExceedsLimit) {
  mode_config fail_mode{
      .mode = aml_pwm::Mode::kOn,
      .regular = {},
  };
  pwm_config fail_cfg{
      .polarity = false,
      // period = 1 second, exceeds the maximum allowed period (343'927'680 ns).
      .period_ns = 1'000'000'000,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail_mode),
      .mode_config_size = sizeof(fail_mode),
  };

  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigInvalidTwoTimerModeTimer2PeriodExceedsLimit) {
  mode_config fail_mode{
      .mode = aml_pwm::Mode::kTwoTimer,
      .two_timer =
          {
              // period = 1 second, exceeds the maximum allowed period (343'927'680 ns).
              .period_ns2 = 1'000'000'000,
          },
  };
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1000,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&fail_mode),
      .mode_config_size = sizeof(fail_mode),
  };

  EXPECT_NE(driver().PwmImplSetConfig(0, &fail_cfg), ZX_OK);
}

TEST_F(AmlPwmDriverTest, SetConfigTest) {
  // Mode::kOff
  mode_config off{
      .mode = Mode::kOff,
      .regular = {},
  };
  pwm_config off_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&off),
      .mode_config_size = sizeof(off),
  };
  EXPECT_OK(driver().PwmImplSetConfig(0, &off_cfg));

  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
    (*mmios[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
  });
  mode_config on{
      .mode = Mode::kOn,
      .regular = {},
  };
  pwm_config on_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&on),
      .mode_config_size = sizeof(on),
  };
  EXPECT_OK(driver().PwmImplSetConfig(0, &on_cfg));  // turn on

  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFDFFFFFA);  // SetMode
  });
  EXPECT_OK(driver().PwmImplSetConfig(0, &off_cfg));
  EXPECT_OK(driver().PwmImplSetConfig(0, &off_cfg));  // same configs

  // Mode::kOn
  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x00000002);  // SetMode
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF80FFFF);  // SetClockDivider
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xF7FFFFFF);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x20000000);  // EnableConst
    (*mmios[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
  });
  EXPECT_OK(driver().PwmImplSetConfig(1, &on_cfg));

  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x08000000);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
    (*mmios[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
  });
  on_cfg.polarity = true;
  on_cfg.period_ns = 1000;
  on_cfg.duty_cycle = 30.0;
  EXPECT_OK(driver().PwmImplSetConfig(1, &on_cfg));  // Change Duty Cycle

  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFEFFFFF5);  // SetMode
  });
  EXPECT_OK(driver().PwmImplSetConfig(1, &off_cfg));  // Change Mode

  // Mode::kDeltaSigma
  WithMmios([](auto mmios) {
    (*mmios[1])[2 * 4].ExpectRead(0x02000000).ExpectWrite(0x00000004);  // SetMode
    (*mmios[1])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
    (*mmios[1])[3 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF0064);  // SetDSSetting
    (*mmios[1])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
    (*mmios[1])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xEFFFFFFF);  // EnableConst
    (*mmios[1])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
  });
  mode_config ds{
      .mode = Mode::kDeltaSigma,
      .delta_sigma =
          {
              .delta = 100,
          },
  };
  pwm_config ds_cfg{
      .polarity = false,
      .period_ns = 1000,
      .duty_cycle = 30.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&ds),
      .mode_config_size = sizeof(ds),
  };
  EXPECT_OK(driver().PwmImplSetConfig(2, &ds_cfg));

  // Mode::kTwoTimer
  WithMmios([](auto mmios) {
    (*mmios[3])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x01000002);  // SetMode
    (*mmios[3])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF80FFFF);  // SetClockDivider
    (*mmios[3])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00130003);  // SetDutyCycle2
    (*mmios[3])[4 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF0302);  // SetTimers
    (*mmios[3])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xF7FFFFFF);  // Invert
    (*mmios[3])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
    (*mmios[3])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
  });
  mode_config timer2{
      .mode = Mode::kTwoTimer,
      .two_timer =
          {
              .period_ns2 = 1000,
              .duty_cycle2 = 80.0,
              .timer1 = 3,
              .timer2 = 2,
          },
  };
  pwm_config timer2_cfg{
      .polarity = false,
      .period_ns = 1000,
      .duty_cycle = 30.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&timer2),
      .mode_config_size = sizeof(timer2),
  };
  EXPECT_OK(driver().PwmImplSetConfig(7, &timer2_cfg));
}

TEST_F(AmlPwmDriverTest, SingleTimerModeClockDividerChange) {
  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
    // Expected clock divider value = 1, raw value of divider register field = 0
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
    (*mmios[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
  });
  mode_config on{
      .mode = Mode::kOn,
      .regular = {},
  };
  pwm_config on_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&on),
      .mode_config_size = sizeof(on),
  };
  EXPECT_OK(driver().PwmImplSetConfig(0, &on_cfg));  // Success

  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
    (*mmios[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x5F460000);  // SetDutyCycle
  });
  on_cfg.period_ns = 1'000'000;                      // Doesn't trigger the divider change.
  EXPECT_OK(driver().PwmImplSetConfig(0, &on_cfg));  // Success

  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF81FF);  // SetClockDivider
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
    (*mmios[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x8EE90000);  // SetDutyCycle
  });
  on_cfg.period_ns = 3'000'000;
  EXPECT_OK(driver().PwmImplSetConfig(0, &on_cfg));  // Success

  on_cfg.period_ns = 1'000'000'000;                         // 1 Hz, exceeds the maximum period
  EXPECT_NE(driver().PwmImplSetConfig(0, &on_cfg), ZX_OK);  // Failure
}

TEST_F(AmlPwmDriverTest, TwoTimerModeClockDividerChange) {
  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x01000002);  // SetMode
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF80FFFF);  // SetClockDivider
    (*mmios[0])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00130003);  // SetDutyCycle2
    (*mmios[0])[4 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF0302);  // SetTimers
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xF7FFFFFF);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
    (*mmios[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
  });
  mode_config timer2{
      .mode = Mode::kTwoTimer,
      .two_timer =
          {
              .period_ns2 = 1000,
              .duty_cycle2 = 80.0,
              .timer1 = 3,
              .timer2 = 2,
          },
  };
  pwm_config timer2_cfg{
      .polarity = false,
      .period_ns = 1000,
      .duty_cycle = 30.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&timer2),
      .mode_config_size = sizeof(timer2),
  };
  EXPECT_OK(driver().PwmImplSetConfig(1, &timer2_cfg));

  WithMmios([](auto mmios) {
    // timer1 needs divider = 2, timer2 needs divider = 1,
    // so the divider = max(2, 1) = 2. The raw value is set to (2 - 1) = 1.
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF81FFFF);  // SetClockDivider
    (*mmios[0])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00090001);  // SetDutyCycle2
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
    (*mmios[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x2ADF6408);  // SetDutyCycle
  });
  timer2_cfg.period_ns = 3'000'000;
  EXPECT_OK(driver().PwmImplSetConfig(1, &timer2_cfg));  // Success

  WithMmios([](auto mmios) {
    // timer1 needs divider = 2, timer2 needs divider = 3,
    // so the divider = max(2, 3) = 3. The raw value is set to (3 - 1) = 2.
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF82FFFF);  // SetClockDivider
    (*mmios[0])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x986F261B);  // SetDutyCycle2
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
    (*mmios[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x1C9442B0);  // SetDutyCycle
  });
  timer2.two_timer.period_ns2 = 6'000'000;
  EXPECT_OK(driver().PwmImplSetConfig(1, &timer2_cfg));  // Success
}

TEST_F(AmlPwmDriverTest, SetConfigFailTest) {
  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
    (*mmios[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
  });
  mode_config on{
      .mode = Mode::kOn,
      .regular = {},
  };
  pwm_config on_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&on),
      .mode_config_size = sizeof(on),
  };
  EXPECT_OK(driver().PwmImplSetConfig(0, &on_cfg));  // Success

  WithMmios([](auto mmios) {
    // Nothing should happen on the register if the input is incorrect.
    (*mmios[0])[2 * 4].VerifyAndClear();
    (*mmios[0])[0 * 4].VerifyAndClear();
  });
  on_cfg.polarity = true;
  on_cfg.duty_cycle = 120.0;
  EXPECT_NE(driver().PwmImplSetConfig(0, &on_cfg), ZX_OK);  // Fail
}

TEST_F(AmlPwmDriverTest, EnableTest) {
  EXPECT_NE(driver().PwmImplEnable(10), ZX_OK);  // Fail

  WithMmios([](auto mmios) { (*mmios[1])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x00008000); });
  EXPECT_OK(driver().PwmImplEnable(2));
  EXPECT_OK(driver().PwmImplEnable(2));  // Enable twice

  WithMmios([](auto mmios) { (*mmios[2])[2 * 4].ExpectRead(0x00008000).ExpectWrite(0x00808000); });
  EXPECT_OK(driver().PwmImplEnable(5));  // Enable other PWMs
}

TEST_F(AmlPwmDriverTest, DisableTest) {
  EXPECT_NE(driver().PwmImplDisable(10), ZX_OK);  // Fail

  EXPECT_OK(driver().PwmImplDisable(0));  // Disable first

  WithMmios([](auto mmios) { (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x00008000); });
  EXPECT_OK(driver().PwmImplEnable(0));

  WithMmios([](auto mmios) { (*mmios[0])[2 * 4].ExpectRead(0x00008000).ExpectWrite(0x00000000); });
  EXPECT_OK(driver().PwmImplDisable(0));
  EXPECT_OK(driver().PwmImplDisable(0));  // Disable twice

  WithMmios([](auto mmios) { (*mmios[2])[2 * 4].ExpectRead(0x00008000).ExpectWrite(0x00808000); });
  EXPECT_OK(driver().PwmImplEnable(5));  // Enable other PWMs

  WithMmios([](auto mmios) { (*mmios[2])[2 * 4].ExpectRead(0x00808000).ExpectWrite(0x00008000); });
  EXPECT_OK(driver().PwmImplDisable(5));  // Disable other PWMs
}

TEST_F(AmlPwmDriverTest, SetConfigPeriodNotDivisibleBy100Test) {
  WithMmios([](auto mmios) {
    (*mmios[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
    (*mmios[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
    (*mmios[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
    (*mmios[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x10420000);  // SetDutyCycle
  });
  mode_config on{
      .mode = Mode::kOn,
      .regular = {},
  };
  pwm_config on_cfg{
      .polarity = false,
      .period_ns = 170625,
      .duty_cycle = 100.0,
      .mode_config_buffer = reinterpret_cast<uint8_t*>(&on),
      .mode_config_size = sizeof(on),
  };
  EXPECT_OK(driver().PwmImplSetConfig(0, &on_cfg));  // Success
}

}  // namespace pwm

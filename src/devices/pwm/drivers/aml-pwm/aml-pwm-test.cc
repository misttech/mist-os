// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-pwm.h"

#include <vector>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <mock-mmio-reg/mock-mmio-reg.h>
#include <soc/aml-common/aml-pwm-regs.h>

namespace {

constexpr size_t kRegSize = 0x00001000 / sizeof(uint32_t);  // in 32 bits chunks.

}  // namespace

namespace pwm {

class FakeAmlPwmDevice : public AmlPwmDevice {
 public:
  static std::unique_ptr<FakeAmlPwmDevice> Create(
      std::vector<fdf::MmioBuffer> mmios, fuchsia_hardware_pwm::PwmChannelsMetadata metadata) {
    fbl::AllocChecker ac;
    auto device = fbl::make_unique_checked<FakeAmlPwmDevice>(&ac);
    if (!ac.check()) {
      zxlogf(ERROR, "%s: device object alloc failed", __func__);
      return nullptr;
    }
    auto status = device->Init(std::move(mmios), std::move(metadata));
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: device object init failed", __func__);
      return nullptr;
    }

    return device;
  }

  explicit FakeAmlPwmDevice() : AmlPwmDevice() {}
};

class AmlPwmDeviceTest : public zxtest::Test {
 public:
  void SetUp() override {
    fbl::AllocChecker ac;

    for (auto& mock_mmio : mock_mmios_) {
      mock_mmio =
          fbl::make_unique_checked<ddk_mock::MockMmioRegRegion>(&ac, sizeof(uint32_t), kRegSize);
      if (!ac.check()) {
        zxlogf(ERROR, "%s: mock_mmio alloc failed", __func__);
        return;
      }
    }

    for (uint32_t i = 0; i < mock_mmios_.size(); i++) {
      // Even numbered channel SetMode calls.
      (*mock_mmios_[i])[2lu * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFDFFFFFA);

      // Odd numbered channel SetMode calls. However, initialization will be skipped for channel 3.
      if (i != 1) {
        (*mock_mmios_[i])[2lu * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFEFFFFF5);
      }
    }

    std::vector<fdf::MmioBuffer> mmios;
    mmios.reserve(mock_mmios_.size());
    for (auto& mock_mmio : mock_mmios_) {
      mmios.push_back(mock_mmio->GetMmioBuffer());
    }

    // Protect channel 3 for protect tests
    fuchsia_hardware_pwm::PwmChannelsMetadata metadata = {{{{{{.id = 0}},
                                                             {{.id = 1}},
                                                             {{.id = 2}},
                                                             {{.id = 3, .skip_init = true}},
                                                             {{.id = 4}},
                                                             {{.id = 5}},
                                                             {{.id = 6}},
                                                             {{.id = 7}},
                                                             {{.id = 8}},
                                                             {{.id = 9}}}}}};

    pwm_ = FakeAmlPwmDevice::Create(std::move(mmios), metadata);
    ASSERT_NOT_NULL(pwm_);
  }

  void TearDown() override {
    for (auto& mock_mmio : mock_mmios_) {
      mock_mmio->VerifyAll();
    }
  }

 protected:
  std::unique_ptr<FakeAmlPwmDevice> pwm_;

  // Mmio Regs and Regions
  std::array<std::unique_ptr<ddk_mock::MockMmioRegRegion>, 5> mock_mmios_;
};

TEST_F(AmlPwmDeviceTest, ProtectTest) {
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
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(3, &cfg));
}

TEST_F(AmlPwmDeviceTest, GetConfigTest) {
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
  EXPECT_OK(pwm_->PwmImplGetConfig(0, &cfg));

  cfg.mode_config_buffer = nullptr;
  EXPECT_NOT_OK(pwm_->PwmImplGetConfig(0, &cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidNullConfig) {
  // config is null
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, nullptr));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidNoModeBuffer) {
  pwm_config fail_cfg{
      .polarity = false,
      .period_ns = 1250,
      .duty_cycle = 100.0,
      // config has no mode buffer
      .mode_config_buffer = nullptr,
      .mode_config_size = 0,
  };
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidModeConfigSizeIncorrect) {
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

  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidTwoTimerTimer2InvalidDutyCycle) {
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
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));

  fail_mode.two_timer.duty_cycle2 = 120.0;
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidTimer1InvalidDutyCycle) {
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
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));

  fail_cfg.duty_cycle = 120.0;
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidTimer1InvalidMode) {
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

  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidPwmId) {
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
    EXPECT_NOT_OK(pwm_->PwmImplSetConfig(10, &fail_cfg));
  }
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidTimer1PeriodExceedsLimit) {
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

  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigInvalidTwoTimerModeTimer2PeriodExceedsLimit) {
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

  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &fail_cfg));
}

TEST_F(AmlPwmDeviceTest, SetConfigTest) {
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
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &off_cfg));

  (*mock_mmios_[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
  (*mock_mmios_[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // turn on

  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFDFFFFFA);  // SetMode
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &off_cfg));
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &off_cfg));  // same configs

  // Mode::kOn
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x00000002);  // SetMode
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF80FFFF);  // SetClockDivider
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xF7FFFFFF);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x20000000);  // EnableConst
  (*mock_mmios_[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
  EXPECT_OK(pwm_->PwmImplSetConfig(1, &on_cfg));

  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x08000000);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
  (*mock_mmios_[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
  on_cfg.polarity = true;
  on_cfg.period_ns = 1000;
  on_cfg.duty_cycle = 30.0;
  EXPECT_OK(pwm_->PwmImplSetConfig(1, &on_cfg));  // Change Duty Cycle

  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFEFFFFF5);  // SetMode
  EXPECT_OK(pwm_->PwmImplSetConfig(1, &off_cfg));                        // Change Mode

  // Mode::kDeltaSigma
  (*mock_mmios_[1])[2 * 4].ExpectRead(0x02000000).ExpectWrite(0x00000004);  // SetMode
  (*mock_mmios_[1])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
  (*mock_mmios_[1])[3 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF0064);  // SetDSSetting
  (*mock_mmios_[1])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
  (*mock_mmios_[1])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xEFFFFFFF);  // EnableConst
  (*mock_mmios_[1])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(2, &ds_cfg));

  // Mode::kTwoTimer
  (*mock_mmios_[3])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x01000002);  // SetMode
  (*mock_mmios_[3])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF80FFFF);  // SetClockDivider
  (*mock_mmios_[3])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00130003);  // SetDutyCycle2
  (*mock_mmios_[3])[4 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF0302);  // SetTimers
  (*mock_mmios_[3])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xF7FFFFFF);  // Invert
  (*mock_mmios_[3])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
  (*mock_mmios_[3])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(7, &timer2_cfg));
}

TEST_F(AmlPwmDeviceTest, SingleTimerModeClockDividerChange) {
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
  // Expected clock divider value = 1, raw value of divider register field = 0
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
  (*mock_mmios_[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Success

  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
  (*mock_mmios_[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x5F460000);  // SetDutyCycle
  on_cfg.period_ns = 1'000'000;                   // Doesn't trigger the divider change.
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Success

  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF81FF);  // SetClockDivider
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
  (*mock_mmios_[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x8EE90000);  // SetDutyCycle
  on_cfg.period_ns = 3'000'000;
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Success

  on_cfg.period_ns = 1'000'000'000;                   // 1 Hz, exceeds the maximum period
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Failure
}

TEST_F(AmlPwmDeviceTest, TwoTimerModeClockDividerChange) {
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x01000002);  // SetMode
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF80FFFF);  // SetClockDivider
  (*mock_mmios_[0])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00130003);  // SetDutyCycle2
  (*mock_mmios_[0])[4 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF0302);  // SetTimers
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xF7FFFFFF);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
  (*mock_mmios_[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00060010);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(1, &timer2_cfg));

  // timer1 needs divider = 2, timer2 needs divider = 1,
  // so the divider = max(2, 1) = 2. The raw value is set to (2 - 1) = 1.
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF81FFFF);  // SetClockDivider
  (*mock_mmios_[0])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x00090001);  // SetDutyCycle2
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
  (*mock_mmios_[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x2ADF6408);  // SetDutyCycle
  timer2_cfg.period_ns = 3'000'000;
  EXPECT_OK(pwm_->PwmImplSetConfig(1, &timer2_cfg));  // Success

  // timer1 needs divider = 2, timer2 needs divider = 3,
  // so the divider = max(2, 3) = 3. The raw value is set to (3 - 1) = 2.
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFF82FFFF);  // SetClockDivider
  (*mock_mmios_[0])[6 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x986F261B);  // SetDutyCycle2
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xDFFFFFFF);  // EnableConst
  (*mock_mmios_[0])[1 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x1C9442B0);  // SetDutyCycle
  timer2.two_timer.period_ns2 = 6'000'000;
  EXPECT_OK(pwm_->PwmImplSetConfig(1, &timer2_cfg));  // Success
}

TEST_F(AmlPwmDeviceTest, SetConfigFailTest) {
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
  (*mock_mmios_[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x001E0000);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Success

  // Nothing should happen on the register if the input is incorrect.
  (*mock_mmios_[0])[2 * 4].VerifyAndClear();
  (*mock_mmios_[0])[0 * 4].VerifyAndClear();
  on_cfg.polarity = true;
  on_cfg.duty_cycle = 120.0;
  EXPECT_NOT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Fail
}

TEST_F(AmlPwmDeviceTest, EnableTest) {
  EXPECT_NOT_OK(pwm_->PwmImplEnable(10));  // Fail

  (*mock_mmios_[1])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x00008000);
  EXPECT_OK(pwm_->PwmImplEnable(2));
  EXPECT_OK(pwm_->PwmImplEnable(2));  // Enable twice

  (*mock_mmios_[2])[2 * 4].ExpectRead(0x00008000).ExpectWrite(0x00808000);
  EXPECT_OK(pwm_->PwmImplEnable(5));  // Enable other PWMs
}

TEST_F(AmlPwmDeviceTest, DisableTest) {
  EXPECT_NOT_OK(pwm_->PwmImplDisable(10));  // Fail

  EXPECT_OK(pwm_->PwmImplDisable(0));  // Disable first

  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x00008000);
  EXPECT_OK(pwm_->PwmImplEnable(0));

  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00008000).ExpectWrite(0x00000000);
  EXPECT_OK(pwm_->PwmImplDisable(0));
  EXPECT_OK(pwm_->PwmImplDisable(0));  // Disable twice

  (*mock_mmios_[2])[2 * 4].ExpectRead(0x00008000).ExpectWrite(0x00808000);
  EXPECT_OK(pwm_->PwmImplEnable(5));  // Enable other PWMs

  (*mock_mmios_[2])[2 * 4].ExpectRead(0x00808000).ExpectWrite(0x00008000);
  EXPECT_OK(pwm_->PwmImplDisable(5));  // Disable other PWMs
}

TEST_F(AmlPwmDeviceTest, SetConfigPeriodNotDivisibleBy100Test) {
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x01000000).ExpectWrite(0x01000001);  // SetMode
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFFFF80FF);  // SetClockDivider
  (*mock_mmios_[0])[2 * 4].ExpectRead(0xFFFFFFFF).ExpectWrite(0xFBFFFFFF);  // Invert
  (*mock_mmios_[0])[2 * 4].ExpectRead(0x00000000).ExpectWrite(0x10000000);  // EnableConst
  (*mock_mmios_[0])[0 * 4].ExpectRead(0xA39D9259).ExpectWrite(0x10420000);  // SetDutyCycle
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
  EXPECT_OK(pwm_->PwmImplSetConfig(0, &on_cfg));  // Success
}

}  // namespace pwm

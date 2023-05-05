// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-pwm.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <limits>

#include <soc/aml-a1/a1-pwm.h>
#include <soc/aml-a113/a113-pwm.h>
#include <soc/aml-a5/a5-pwm.h>
#include <soc/aml-common/aml-pwm-regs.h>
#include <soc/aml-s905d2/s905d2-pwm.h>
#include <soc/aml-t931/t931-pwm.h>

namespace pwm {

namespace {

// Input clock frequency
constexpr uint32_t kReferenceClockFrequencyHz = 24'000'000;
// Nanoseconds per second
constexpr uint32_t kNsecPerSec = 1'000'000'000;

constexpr int64_t DivideRounded(int64_t num, int64_t denom) { return (num + (denom / 2)) / denom; }

constexpr int kMinimumDivider = 1;
constexpr int kMaximumDivider = 0x7f + 1;

// Below explains how `kMaximumAllowedPeriodNs` is calculated and we statically
// verify the value.
//
// High / low time of a PWM period is stored in registers as numbers of clock
// periods and dividers:
//
//   High time (ns) = <NUMBER OF CLOCK PERIODS> (16 bits unsigned)
//                  * (Nanoseconds per clock period)
//                  * (<DIVIDER> + 1) (<DIVIDER> is 7-bit unsigned)
//
// So the maximum allowed PWM period is the maximum value of High (low) time.
//
//   0xffff * (Nanoseconds per clock period) * (0x7f + 1)  (unit: ns)
static_assert([] {
  constexpr int64_t kNanosecondsPerClock = kNsecPerSec / kReferenceClockFrequencyHz;
  constexpr int kMaxClockPeriodCount = std::numeric_limits<uint16_t>::max();

  return kMaximumAllowedPeriodNs == kNanosecondsPerClock * kMaximumDivider * kMaxClockPeriodCount;
}());

// Gets the minimum allowed divider value from given `period_ns`.
//
// `period_ns` must be not greater than kMaximumAllowedPeriodNs.
int GetDividerFromPeriodNs(int64_t period_ns) {
  ZX_ASSERT(period_ns <= kMaximumAllowedPeriodNs);
  constexpr int64_t kNanosecondsPerClock = kNsecPerSec / kReferenceClockFrequencyHz;
  for (int divider = kMinimumDivider; divider <= kMaximumDivider; divider++) {
    const int64_t period_count = DivideRounded(period_ns, kNanosecondsPerClock * divider);
    if (period_count <= std::numeric_limits<uint16_t>::max()) {
      return divider;
    }
  }
  // Given that period_ns never exceeds the maximum allowed value, this should
  // never happen.
  ZX_ASSERT(false);
}

// Gets the minimum allowed divider value from given `config`, fulfilling
// requirements for all the timers.
//
// `period_ns` and `period_ns2` (for two-timer mode) must be not greater than
// kMaximumAllowedPeriodNs.
int GetDividerFromConfig(const pwm_config_t* config) {
  ZX_ASSERT(config);
  ZX_ASSERT(config->mode_config_buffer);
  const auto* mode_cfg = reinterpret_cast<const mode_config*>(config->mode_config_buffer);
  switch (mode_cfg->mode) {
    case Mode::kOff:
      return 1;
    case Mode::kOn:
    case Mode::kDeltaSigma: {
      ZX_ASSERT(config->period_ns <= kMaximumAllowedPeriodNs);
      return GetDividerFromPeriodNs(config->period_ns);
    }
    case Mode::kTwoTimer: {
      ZX_ASSERT(config->period_ns <= kMaximumAllowedPeriodNs);
      ZX_ASSERT(mode_cfg->two_timer.period_ns2 <= kMaximumAllowedPeriodNs);
      int divider_first_timer = GetDividerFromPeriodNs(config->period_ns);
      int divider_second_timer = GetDividerFromPeriodNs(mode_cfg->two_timer.period_ns2);
      return std::max(divider_first_timer, divider_second_timer);
    }
  }
}

struct DutyCycleClockCount {
  int64_t high_count;
  int64_t low_count;
};

DutyCycleClockCount DutyCycleToClockCount(int divider, float duty_cycle, int64_t period_ns) {
  constexpr int64_t kNanosecondsPerClock = kNsecPerSec / kReferenceClockFrequencyHz;

  // Calculate the high and low count first based on the duty cycle requested.
  const int64_t high_time_ns = DivideRounded((duty_cycle * static_cast<float>(period_ns)), 100.0);
  const int64_t period_count = DivideRounded(period_ns, kNanosecondsPerClock * divider);
  const int64_t duty_count = DivideRounded(high_time_ns, kNanosecondsPerClock * divider);

  int64_t high_count = duty_count;
  int64_t low_count = period_count - duty_count;
  if (duty_count != period_count && duty_count != 0) {
    high_count--;
    low_count--;
  }

  return DutyCycleClockCount{
      .high_count = high_count,
      .low_count = low_count,
  };
}

bool IsValidConfig(const pwm_config_t* config) {
  if (config == nullptr) {
    zxlogf(ERROR, "config is null");
    return false;
  }
  if (config->mode_config_buffer == nullptr) {
    zxlogf(ERROR, "mode_config_buffer not found");
    return false;
  }
  if (config->mode_config_size != sizeof(mode_config)) {
    zxlogf(ERROR, "mode_config_size incorrect: expected %lu, actual %lu", sizeof(mode_config),
           config->mode_config_size);
    return false;
  }

  auto mode_cfg = reinterpret_cast<const mode_config*>(config->mode_config_buffer);
  Mode mode = mode_cfg->mode;
  switch (mode) {
    case Mode::kOff:
      return true;
    case Mode::kTwoTimer:
      if (!(mode_cfg->two_timer.duty_cycle2 >= 0.0f && mode_cfg->two_timer.duty_cycle2 <= 100.0f)) {
        zxlogf(ERROR, "timer #2 duty cycle (%0.3f) is not in [0.0, 100.0]",
               mode_cfg->two_timer.duty_cycle2);
        return false;
      }
      if (mode_cfg->two_timer.period_ns2 > kMaximumAllowedPeriodNs) {
        zxlogf(ERROR, "timer #2 period (%u ns) exceeds the maximum allowed period (%ld ns)",
               mode_cfg->two_timer.period_ns2, kMaximumAllowedPeriodNs);
        return false;
      }
      [[fallthrough]];
    case Mode::kOn:
    case Mode::kDeltaSigma:
      if (!(config->duty_cycle >= 0.0f && config->duty_cycle <= 100.0f)) {
        zxlogf(ERROR, "timer #1 duty cycle (%0.3f) is not in [0.0, 100.0]", config->duty_cycle);
        return false;
      }
      if (config->period_ns > kMaximumAllowedPeriodNs) {
        zxlogf(ERROR, "timer #1 period (%u ns) exceeds the maximum allowed period (%ld ns)",
               config->period_ns, kMaximumAllowedPeriodNs);
        return false;
      }
      break;
    default:
      zxlogf(ERROR, "Unsupported mode (%u)", static_cast<uint32_t>(mode));
      return false;
  }
  return true;
}

void CopyConfig(pwm_config_t* dest, const pwm_config_t* src) {
  ZX_DEBUG_ASSERT(dest->mode_config_buffer);
  ZX_DEBUG_ASSERT(dest->mode_config_size == src->mode_config_size);
  ZX_DEBUG_ASSERT(dest->mode_config_size == sizeof(mode_config));

  dest->polarity = src->polarity;
  dest->period_ns = src->period_ns;
  dest->duty_cycle = src->duty_cycle;
  memcpy(dest->mode_config_buffer, src->mode_config_buffer, src->mode_config_size);
}

}  // namespace

zx_status_t AmlPwm::PwmImplGetConfig(uint32_t idx, pwm_config_t* out_config) {
  if (idx > 1) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (out_config->mode_config_buffer == nullptr ||
      out_config->mode_config_size != configs_[idx].mode_config_size ||
      out_config->mode_config_size != sizeof(mode_config)) {
    return ZX_ERR_INVALID_ARGS;
  }
  CopyConfig(out_config, &configs_[idx]);
  return ZX_OK;
}

zx_status_t AmlPwm::PwmImplSetConfig(uint32_t idx, const pwm_config_t* config) {
  if (idx > 1) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (!IsValidConfig(config)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Save old config
  mode_config tmp_cfg = {Mode::kOff, {}};
  pwm_config_t old_config = {false, 0, 0.0, reinterpret_cast<uint8_t*>(&tmp_cfg),
                             sizeof(mode_config)};
  CopyConfig(&old_config, &configs_[idx]);
  auto old_mode_cfg = reinterpret_cast<const mode_config*>(old_config.mode_config_buffer);

  // Update new
  CopyConfig(&configs_[idx], config);

  auto mode_cfg = reinterpret_cast<const mode_config*>(config->mode_config_buffer);
  Mode mode = mode_cfg->mode;

  bool mode_eq = (old_mode_cfg->mode == mode);
  if (!mode_eq) {
    SetMode(idx, mode);
  }

  if (mode == Mode::kOff) {
    return ZX_OK;
  }

  int old_divider = GetDividerFromConfig(&old_config);
  int new_divider = GetDividerFromConfig(config);
  bool divider_eq = old_divider == new_divider;
  if (!(mode_eq && divider_eq)) {
    SetClockDivider(idx, new_divider);
  }

  bool en_const = (config->duty_cycle == 0 || config->duty_cycle == 100);
  bool val_eq;

  if (mode == Mode::kDeltaSigma) {
    val_eq = (old_mode_cfg->delta_sigma.delta == mode_cfg->delta_sigma.delta);
    if (!(mode_eq && val_eq)) {
      SetDSSetting(idx, mode_cfg->delta_sigma.delta);
    }
  }

  if (mode == Mode::kTwoTimer) {
    en_const = (en_const || mode_cfg->two_timer.duty_cycle2 == 0 ||
                mode_cfg->two_timer.duty_cycle2 == 100);

    val_eq = (old_mode_cfg->two_timer.period_ns2 == mode_cfg->two_timer.period_ns2) &&
             (old_mode_cfg->two_timer.duty_cycle2 == mode_cfg->two_timer.duty_cycle2);
    if (!(mode_eq && divider_eq && val_eq)) {
      SetDutyCycle2(idx, new_divider, mode_cfg->two_timer.period_ns2,
                    mode_cfg->two_timer.duty_cycle2);
    }

    val_eq = (old_mode_cfg->two_timer.timer1 == mode_cfg->two_timer.timer1) &&
             (old_mode_cfg->two_timer.timer2 == mode_cfg->two_timer.timer2);
    if (!(mode_eq && val_eq)) {
      SetTimers(idx, mode_cfg->two_timer.timer1, mode_cfg->two_timer.timer2);
    }
  }

  val_eq = (old_config.polarity == config->polarity);
  if (!(mode_eq && val_eq)) {
    Invert(idx, config->polarity);
  }
  EnableConst(idx, en_const);

  val_eq =
      (old_config.period_ns == config->period_ns) && (old_config.duty_cycle == config->duty_cycle);
  if (!(mode_eq && divider_eq && val_eq)) {
    SetDutyCycle(idx, new_divider, config->period_ns, config->duty_cycle);
  }

  return ZX_OK;
}

zx_status_t AmlPwm::PwmImplEnable(uint32_t idx) {
  if (idx > 1) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!enabled_[idx]) {
    EnableClock(idx, true);
    enabled_[idx] = true;
  }
  return ZX_OK;
}

zx_status_t AmlPwm::PwmImplDisable(uint32_t idx) {
  if (idx > 1) {
    return ZX_ERR_INVALID_ARGS;
  }
  zx_status_t status = ZX_OK;
  if (enabled_[idx]) {
    EnableClock(idx, false);
    enabled_[idx] = false;
  }
  return status;
}

void AmlPwm::SetMode(uint32_t idx, Mode mode) {
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_en_b(mode == Mode::kOn || mode == Mode::kTwoTimer)
        .set_ds_en_b(mode == Mode::kDeltaSigma)
        .set_en_b2(mode == Mode::kTwoTimer);
  } else {
    misc_reg.set_en_a(mode == Mode::kOn || mode == Mode::kTwoTimer)
        .set_ds_en_a(mode == Mode::kDeltaSigma)
        .set_en_a2(mode == Mode::kTwoTimer);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::SetDutyCycle(uint32_t idx, int divider, uint32_t period_ns, float duty_cycle) {
  ZX_ASSERT(duty_cycle >= 0.0f);
  ZX_ASSERT(duty_cycle <= 100.0f);
  ZX_ASSERT(divider >= 1);
  ZX_ASSERT(divider <= 128);

  // Write duty cycle to registers
  DutyCycleClockCount clock_count = DutyCycleToClockCount(divider, duty_cycle, period_ns);
  if (clock_count.high_count < std::numeric_limits<uint16_t>::min() ||
      clock_count.high_count > std::numeric_limits<uint16_t>::max()) {
    zxlogf(ERROR, "Invalid high count: %ld", clock_count.high_count);
  }
  if (clock_count.low_count < std::numeric_limits<uint16_t>::min() ||
      clock_count.low_count > std::numeric_limits<uint16_t>::max()) {
    zxlogf(ERROR, "Invalid low count: %ld", clock_count.low_count);
  }

  if (idx % 2) {
    fbl::AutoLock lock(&locks_[REG_B]);
    DutyCycleReg::GetB()
        .ReadFrom(&mmio_)
        .set_high(clock_count.high_count)
        .set_low(clock_count.low_count)
        .WriteTo(&mmio_);
  } else {
    fbl::AutoLock lock(&locks_[REG_A]);
    DutyCycleReg::GetA()
        .ReadFrom(&mmio_)
        .set_high(clock_count.high_count)
        .set_low(clock_count.low_count)
        .WriteTo(&mmio_);
  }
}

void AmlPwm::SetDutyCycle2(uint32_t idx, int divider, uint32_t period_ns, float duty_cycle) {
  ZX_ASSERT(duty_cycle >= 0.0f);
  ZX_ASSERT(duty_cycle <= 100.0f);
  ZX_ASSERT(divider >= 1);
  ZX_ASSERT(divider <= 128);

  // Write duty cycle to registers
  DutyCycleClockCount clock_count = DutyCycleToClockCount(divider, duty_cycle, period_ns);
  if (clock_count.high_count < std::numeric_limits<uint16_t>::min() ||
      clock_count.high_count > std::numeric_limits<uint16_t>::max()) {
    zxlogf(ERROR, "Invalid high count: %ld", clock_count.high_count);
  }
  if (clock_count.low_count < std::numeric_limits<uint16_t>::min() ||
      clock_count.low_count > std::numeric_limits<uint16_t>::max()) {
    zxlogf(ERROR, "Invalid low count: %ld", clock_count.low_count);
  }

  if (idx % 2) {
    fbl::AutoLock lock(&locks_[REG_B2]);
    DutyCycleReg::GetB2()
        .ReadFrom(&mmio_)
        .set_high(clock_count.high_count)
        .set_low(clock_count.low_count)
        .WriteTo(&mmio_);
  } else {
    fbl::AutoLock lock(&locks_[REG_A2]);
    DutyCycleReg::GetA2()
        .ReadFrom(&mmio_)
        .set_high(clock_count.high_count)
        .set_low(clock_count.low_count)
        .WriteTo(&mmio_);
  }
}

void AmlPwm::Invert(uint32_t idx, bool on) {
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_inv_en_b(on);
  } else {
    misc_reg.set_inv_en_a(on);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::EnableHiZ(uint32_t idx, bool on) {
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_hiz_b(on);
  } else {
    misc_reg.set_hiz_a(on);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::EnableClock(uint32_t idx, bool on) {
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_clk_en_b(on);
  } else {
    misc_reg.set_clk_en_a(on);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::EnableConst(uint32_t idx, bool on) {
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_constant_en_b(on);
  } else {
    misc_reg.set_constant_en_a(on);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::SetClock(uint32_t idx, uint8_t sel) {
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_clk_sel_b(sel);
  } else {
    misc_reg.set_clk_sel_a(sel);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::SetClockDivider(uint32_t idx, int divider) {
  ZX_ASSERT(divider >= 1);
  ZX_ASSERT(divider <= 128);
  uint8_t divider_select = divider - 1;
  fbl::AutoLock lock(&locks_[REG_MISC]);
  auto misc_reg = MiscReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    misc_reg.set_clk_div_b(divider_select);
  } else {
    misc_reg.set_clk_div_a(divider_select);
  }
  misc_reg.WriteTo(&mmio_);
}

void AmlPwm::EnableBlink(uint32_t idx, bool on) {
  fbl::AutoLock lock(&locks_[REG_BLINK]);
  auto blink_reg = BlinkReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    blink_reg.set_enable_b(on);
  } else {
    blink_reg.set_enable_a(on);
  }
  blink_reg.WriteTo(&mmio_);
}

void AmlPwm::SetBlinkTimes(uint32_t idx, uint8_t times) {
  fbl::AutoLock lock(&locks_[REG_BLINK]);
  auto blink_reg = BlinkReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    blink_reg.set_times_b(times);
  } else {
    blink_reg.set_times_a(times);
  }
  blink_reg.WriteTo(&mmio_);
}

void AmlPwm::SetDSSetting(uint32_t idx, uint16_t val) {
  fbl::AutoLock lock(&locks_[REG_DS]);
  auto ds_reg = DeltaSigmaReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    ds_reg.set_b(val);
  } else {
    ds_reg.set_a(val);
  }
  ds_reg.WriteTo(&mmio_);
}

void AmlPwm::SetTimers(uint32_t idx, uint8_t timer1, uint8_t timer2) {
  fbl::AutoLock lock(&locks_[REG_TIME]);
  auto time_reg = TimeReg::Get().ReadFrom(&mmio_);
  if (idx % 2) {
    time_reg.set_b1(timer1).set_b2(timer2);
  } else {
    time_reg.set_a1(timer1).set_a2(timer2);
  }
  time_reg.WriteTo(&mmio_);
}

zx_status_t AmlPwmDevice::Create(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  std::unique_ptr<AmlPwmDevice> device(new (&ac) AmlPwmDevice(parent));
  if (!ac.check()) {
    zxlogf(ERROR, "%s: device object alloc failed", __func__);
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = ZX_OK;
  if ((status = device->Init(parent)) != ZX_OK) {
    zxlogf(ERROR, "%s: Init failed", __func__);
    return status;
  }

  if (auto status = device->DdkAdd(ddk::DeviceAddArgs("aml-pwm-device")
                                       .set_proto_id(ZX_PROTOCOL_PWM_IMPL)
                                       .forward_metadata(parent, DEVICE_METADATA_PWM_IDS));
      status != ZX_OK) {
    zxlogf(ERROR, "%s: DdkAdd failed", __func__);
    return status;
  }

  [[maybe_unused]] auto* unused = device.release();

  return ZX_OK;
}

zx_status_t AmlPwmDevice::Init(zx_device_t* parent) {
  zx_status_t status = ZX_OK;
  auto pwm_ids = ddk::GetMetadataArray<pwm_id_t>(parent, DEVICE_METADATA_PWM_IDS);
  if (!pwm_ids.is_ok()) {
    return pwm_ids.error_value();
  }

  ddk::PDevFidl pdev(parent);
  for (uint32_t i = 0;; i++) {
    std::optional<fdf::MmioBuffer> mmio;
    if ((status = pdev.MapMmio(i, &mmio)) != ZX_OK) {
      break;
    }
    pwms_.push_back(std::make_unique<AmlPwm>(*std::move(mmio), pwm_ids.value()[2 * i],
                                             pwm_ids.value()[2 * i + 1]));
    pwms_.back()->Init();
  }

  return ZX_OK;
}

zx_status_t AmlPwmDevice::PwmImplGetConfig(uint32_t idx, pwm_config_t* out_config) {
  if (idx >= pwms_.size() * 2 || out_config == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  return pwms_[idx / 2]->PwmImplGetConfig(idx % 2, out_config);
}

zx_status_t AmlPwmDevice::PwmImplSetConfig(uint32_t idx, const pwm_config_t* config) {
  if (idx >= pwms_.size() * 2 || config == nullptr || config->mode_config_buffer == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  return pwms_[idx / 2]->PwmImplSetConfig(idx % 2, config);
}

zx_status_t AmlPwmDevice::PwmImplEnable(uint32_t idx) {
  if (idx >= pwms_.size() * 2) {
    return ZX_ERR_INVALID_ARGS;
  }
  return pwms_[idx / 2]->PwmImplEnable(idx % 2);
}

zx_status_t AmlPwmDevice::PwmImplDisable(uint32_t idx) {
  if (idx >= pwms_.size() * 2) {
    return ZX_ERR_INVALID_ARGS;
  }
  return pwms_[idx / 2]->PwmImplDisable(idx % 2);
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlPwmDevice::Create;
  return ops;
}();

}  // namespace pwm

ZIRCON_DRIVER(pwm, pwm::driver_ops, "zircon", "0.1");

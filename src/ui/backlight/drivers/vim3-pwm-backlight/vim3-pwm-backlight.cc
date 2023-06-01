// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/backlight/drivers/vim3-pwm-backlight/vim3-pwm-backlight.h"

#include <fidl/fuchsia.hardware.backlight/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <zircon/assert.h>

#include <cmath>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>
#include <soc/aml-common/aml-pwm-regs.h>

namespace vim3_pwm_backlight {

namespace {

// These values are dumped from VIM3 device tree configuration.
constexpr bool kDefaultPolarity = false;
constexpr int64_t kDefaultFrequencyHz = 180;

constexpr int64_t kNanosecondsPerSecond = 1'000'000'000;
constexpr int64_t kDefaultPeriodNs = kNanosecondsPerSecond / kDefaultFrequencyHz;
static_assert(kDefaultPeriodNs <= aml_pwm::kMaximumAllowedPeriodNs);

constexpr float kMaxDutyCycle = 100.0f;
constexpr float kMinDutyCycle = 0.0f;
static_assert(kMaxDutyCycle <= aml_pwm::kMaximumAllowedDutyCycle);
static_assert(kMinDutyCycle >= aml_pwm::kMinimumAllowedDutyCycle);

constexpr double kMaxNormalizedBrightness = 1.0;
constexpr double kMinNormalizedBrightness = 0.0;
static_assert(kMaxNormalizedBrightness > kMinNormalizedBrightness);

float NormalizedBrightnessToDutyCycle(double normalized_brightness) {
  return static_cast<float>(normalized_brightness /
                            (kMaxNormalizedBrightness - kMinNormalizedBrightness) *
                            (kMaxDutyCycle - kMinDutyCycle)) +
         kMinDutyCycle;
}

constexpr struct Vim3PwmBacklight::State kInitialState = {
    .power = true,
    .brightness = 1.0,
};

}  // namespace

void Vim3PwmBacklight::DdkRelease() { delete this; }

void Vim3PwmBacklight::GetStateNormalized(GetStateNormalizedCompleter::Sync& completer) {
  fuchsia_hardware_backlight::wire::State state = {
      .backlight_on = state_.power,
      .brightness = state_.brightness,
  };
  completer.ReplySuccess(state);
}

void Vim3PwmBacklight::SetStateNormalized(SetStateNormalizedRequestView request,
                                          SetStateNormalizedCompleter::Sync& completer) {
  if (request->state.brightness > kMaxNormalizedBrightness) {
    zxlogf(ERROR, "target brightness %0.3f exceeds the maximum brightness limit %0.3f",
           request->state.brightness, kMaxNormalizedBrightness);
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (request->state.brightness < kMinNormalizedBrightness) {
    zxlogf(ERROR, "target brightness %0.3f is lower than the brightness limit %0.3f",
           request->state.brightness, kMinNormalizedBrightness);
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  State target = {
      .power = request->state.backlight_on,
      .brightness = request->state.brightness,
  };
  zx_status_t status = SetState(target);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

zx_status_t Vim3PwmBacklight::SetPwmConfig(bool enabled, float duty_cycle) {
  ZX_ASSERT(pwm_proto_client_.is_valid());
  ZX_ASSERT(duty_cycle >= aml_pwm::kMinimumAllowedDutyCycle);
  ZX_ASSERT(duty_cycle <= aml_pwm::kMaximumAllowedDutyCycle);

  const aml_pwm::Mode mode = enabled ? aml_pwm::Mode::kOn : aml_pwm::Mode::kOff;
  aml_pwm::mode_config mode_config = {
      .mode = mode,
      .regular = {},
  };
  fuchsia_hardware_pwm::wire::PwmConfig config = {
      .polarity = kDefaultPolarity,
      .period_ns = kDefaultPeriodNs,
      .duty_cycle = duty_cycle,
      .mode_config = fidl::VectorView<uint8_t>::FromExternal(
          reinterpret_cast<uint8_t*>(&mode_config), sizeof(mode_config)),
  };

  auto result = pwm_proto_client_->SetConfig(config);
  if (!result.ok()) {
    zxlogf(ERROR, "Cannot set PWM config: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Cannot set PWM config: %s", zx_status_get_string(result.value().error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t Vim3PwmBacklight::InitializeGpioBacklightPower(bool initially_enabled) {
  ZX_ASSERT(!gpio_initialized_);
  zx_status_t status = gpio_backlight_power_.ConfigOut(initially_enabled);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Cannot configure GPIO pin: %s", zx_status_get_string(status));
    return status;
  }
  gpio_initialized_ = true;
  return ZX_OK;
}

zx_status_t Vim3PwmBacklight::SetGpioBacklightPower(bool enabled) {
  ZX_ASSERT(gpio_initialized_);
  zx_status_t status = gpio_backlight_power_.Write(enabled);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Cannot write to GPIO pin: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void Vim3PwmBacklight::StoreState(State state) {
  state_ = state;
  power_property_.Set(state.power);
  brightness_property_.Set(state.brightness);
}

zx_status_t Vim3PwmBacklight::Initialize() {
  if (zx_status_t status = InitializeGpioBacklightPower(/*initially_enabled=*/kInitialState.power);
      status != ZX_OK) {
    zxlogf(ERROR, "Cannot initialize LCD-backlight-enable GPIO pin: %s",
           zx_status_get_string(status));
    return status;
  }
  const float duty_cycle = NormalizedBrightnessToDutyCycle(kInitialState.brightness);
  if (zx_status_t status = SetPwmConfig(/*enabled=*/kInitialState.power, duty_cycle);
      status != ZX_OK) {
    zxlogf(ERROR, "Cannot initialize PWM device: %s", zx_status_get_string(status));
    return status;
  }
  StoreState(kInitialState);
  return ZX_OK;
}

zx_status_t Vim3PwmBacklight::SetState(State target) {
  ZX_ASSERT(target.brightness >= kMinNormalizedBrightness);
  ZX_ASSERT(target.brightness <= kMaxNormalizedBrightness);

  // Change hardware configuration only when target state changes.
  if (target == state_) {
    return ZX_OK;
  }

  auto bailout_gpio = fit::defer([this, power = state_.power] {
    if (zx_status_t status = SetGpioBacklightPower(/*enabled=*/power); status != ZX_OK) {
      zxlogf(ERROR, "Failed to bailout GPIO: %s", zx_status_get_string(status));
    }
  });
  if (zx_status_t status = SetGpioBacklightPower(/*enabled=*/target.power); status != ZX_OK) {
    zxlogf(ERROR, "Cannot set LCD-backlight-enable GPIO pin: %s", zx_status_get_string(status));
    return status;
  }

  auto bailout_pwm = fit::defer([this, power = state_.power, brightness = state_.brightness] {
    const float duty_cycle = NormalizedBrightnessToDutyCycle(brightness);
    if (zx_status_t status = SetPwmConfig(/*enabled=*/power, duty_cycle); status != ZX_OK) {
      zxlogf(ERROR, "Failed to bailout PWM config: %s", zx_status_get_string(status));
    }
  });
  const float duty_cycle = NormalizedBrightnessToDutyCycle(target.brightness);
  if (zx_status_t status = SetPwmConfig(/*enabled=*/target.power, duty_cycle); status != ZX_OK) {
    zxlogf(ERROR, "Cannot set PWM config: %s", zx_status_get_string(status));
    return status;
  }
  StoreState(target);
  bailout_gpio.cancel();
  bailout_pwm.cancel();
  return ZX_OK;
}

zx_status_t Vim3PwmBacklight::Bind() {
  root_ = inspector_.GetRoot().CreateChild("vim3-pwm-backlight");

  zx::result client_end = DdkConnectFragmentFidlProtocol<fuchsia_hardware_pwm::Service::Pwm>("pwm");
  if (client_end.is_error()) {
    zxlogf(ERROR, "Unable to connect to fidl protocol - status: %s", client_end.status_string());
    return client_end.status_value();
  }
  pwm_proto_client_.Bind(std::move(client_end.value()));

  gpio_backlight_power_ = ddk::GpioProtocolClient(parent_, "gpio-lcd-backlight-enable");
  if (!gpio_backlight_power_.is_valid()) {
    return ZX_ERR_PROTOCOL_NOT_SUPPORTED;
  }

  if (zx_status_t status = Initialize(); status != ZX_OK) {
    return status;
  }

  brightness_property_ = root_.CreateDouble("brightness", state_.brightness);
  power_property_ = root_.CreateBool("power", state_.power);

  zx_status_t status =
      DdkAdd(ddk::DeviceAddArgs("vim3-pwm-backlight").set_inspect_vmo(InspectVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Cannot add device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

// static
zx_status_t Vim3PwmBacklight::Create(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto dev = fbl::make_unique_checked<Vim3PwmBacklight>(&ac, parent);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (zx_status_t status = dev->Bind(); status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of releasing memory for dev
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

static constexpr zx_driver_ops_t vim3_pwm_backlight_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = Vim3PwmBacklight::Create;
  return ops;
}();

}  // namespace vim3_pwm_backlight

ZIRCON_DRIVER(vim3_pwm_backlight, vim3_pwm_backlight::vim3_pwm_backlight_driver_ops, "Khadas",
              "0.1");

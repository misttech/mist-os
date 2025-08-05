// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/backlight/drivers/vim3-pwm-backlight/vim3-pwm-backlight.h"

#include <fidl/fuchsia.hardware.backlight/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/fit/defer.h>
#include <zircon/assert.h>

#include <cmath>

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
    fdf::error("target brightness {:.3} exceeds the maximum brightness limit {:.3}",
               request->state.brightness, kMaxNormalizedBrightness);
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  if (request->state.brightness < kMinNormalizedBrightness) {
    fdf::error("target brightness {:.3} is lower than the brightness limit {:.3}",
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
    fdf::error("Cannot set PWM config: {}", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    fdf::error("Cannot set PWM config: {}", zx_status_get_string(result.value().error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t Vim3PwmBacklight::SetGpioBacklightPower(bool enabled) {
  fidl::WireResult result =
      gpio_backlight_power_->SetBufferMode(enabled ? fuchsia_hardware_gpio::BufferMode::kOutputHigh
                                                   : fuchsia_hardware_gpio::BufferMode::kOutputLow);
  if (!result.ok()) {
    fdf::error("Failed to send SetBufferMode request to gpio: {}", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    fdf::error("Failed to configure gpio to output: {}",
               zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

void Vim3PwmBacklight::StoreState(State state) {
  state_ = state;
  power_property_.Set(state.power);
  brightness_property_.Set(state.brightness);
}

zx_status_t Vim3PwmBacklight::Initialize() {
  if (zx_status_t status = SetGpioBacklightPower(/*enabled=*/kInitialState.power);
      status != ZX_OK) {
    fdf::error("Cannot initialize LCD-backlight-enable GPIO pin: {}", zx_status_get_string(status));
    return status;
  }
  const float duty_cycle = NormalizedBrightnessToDutyCycle(kInitialState.brightness);
  if (zx_status_t status = SetPwmConfig(/*enabled=*/kInitialState.power, duty_cycle);
      status != ZX_OK) {
    fdf::error("Cannot initialize PWM device: {}", zx_status_get_string(status));
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
      fdf::error("Failed to bailout GPIO: {}", zx_status_get_string(status));
    }
  });
  if (zx_status_t status = SetGpioBacklightPower(/*enabled=*/target.power); status != ZX_OK) {
    fdf::error("Cannot set LCD-backlight-enable GPIO pin: {}", zx_status_get_string(status));
    return status;
  }

  auto bailout_pwm = fit::defer([this, power = state_.power, brightness = state_.brightness] {
    const float duty_cycle = NormalizedBrightnessToDutyCycle(brightness);
    if (zx_status_t status = SetPwmConfig(/*enabled=*/power, duty_cycle); status != ZX_OK) {
      fdf::error("Failed to bailout PWM config: {}", zx_status_get_string(status));
    }
  });
  const float duty_cycle = NormalizedBrightnessToDutyCycle(target.brightness);
  if (zx_status_t status = SetPwmConfig(/*enabled=*/target.power, duty_cycle); status != ZX_OK) {
    fdf::error("Cannot set PWM config: {}", zx_status_get_string(status));
    return status;
  }
  StoreState(target);
  bailout_gpio.cancel();
  bailout_pwm.cancel();
  return ZX_OK;
}

zx::result<> Vim3PwmBacklight::Start() {
  root_ = inspector().root().CreateChild("vim3-pwm-backlight");

  zx::result pwm_client_end = incoming()->Connect<fuchsia_hardware_pwm::Service::Pwm>("pwm");
  if (pwm_client_end.is_error()) {
    fdf::error("Unable to connect to fidl protocol - status: {}", pwm_client_end);
    return pwm_client_end.take_error();
  }
  pwm_proto_client_.Bind(std::move(pwm_client_end.value()));

  const char* kGpioLcdBacklightEnableParentName = "gpio-lcd-backlight-enable";
  zx::result gpio_client_end = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(
      kGpioLcdBacklightEnableParentName);
  if (gpio_client_end.is_error()) {
    fdf::error("Failed to get gpio protocol from fragment {}: {}",
               kGpioLcdBacklightEnableParentName, gpio_client_end);
    return gpio_client_end.take_error();
  }
  gpio_backlight_power_.Bind(std::move(gpio_client_end.value()));

  if (zx_status_t status = Initialize(); status != ZX_OK) {
    return zx::error(status);
  }

  brightness_property_ = root_.CreateDouble("brightness", state_.brightness);
  power_property_ = root_.CreateBool("power", state_.power);

  fuchsia_hardware_backlight::Service::InstanceHandler backlight_instance_handler{
      {.backlight = bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure)}};
  if (zx::result result = outgoing()->AddService<fuchsia_hardware_backlight::Service>(
          std::move(backlight_instance_handler));
      result.is_error()) {
    fdf::error("Failed to add backlight service: {}", result);
    return result.take_error();
  }

  return zx::ok();
}

}  // namespace vim3_pwm_backlight

FUCHSIA_DRIVER_EXPORT(vim3_pwm_backlight::Vim3PwmBacklight);

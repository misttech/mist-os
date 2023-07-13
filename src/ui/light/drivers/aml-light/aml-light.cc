// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-light.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <string.h>
#include <zircon/errors.h>

#include <cmath>

#include <ddk/metadata/lights.h>
#include <ddktl/fidl.h>
#include <fbl/alloc_checker.h>

namespace aml_light {

namespace {

constexpr double kMaxBrightness = 1.0;
constexpr double kMinBrightness = 0.0;
constexpr zx::duration kPwmPeriod = zx::nsec(170'625);
constexpr zx::duration kNelsonPwmPeriod = zx::nsec(500'000);
static_assert(kPwmPeriod.to_nsecs() <= UINT32_MAX);
static_assert(kNelsonPwmPeriod.to_nsecs() <= UINT32_MAX);

}  // namespace

zx_status_t LightDevice::Init(bool init_on) {
  if (pwm_.has_value()) {
    auto result = (*pwm_)->Enable();
    if (!result.ok()) {
      zxlogf(ERROR, "PWM enable failed: %s", result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "PWM enable failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  return pwm_.has_value() ? SetBrightnessValue(init_on ? kMaxBrightness : kMinBrightness)
                          : SetSimpleValue(init_on);
}

zx_status_t LightDevice::SetSimpleValue(bool value) {
  if (pwm_.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  fidl::WireResult result = gpio_->Write(value);
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send Write request to gpio: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to write to gpio: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  value_ = value;
  return ZX_OK;
}

zx_status_t LightDevice::SetBrightnessValue(double value) {
  if (!pwm_.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  if ((value > kMaxBrightness) || (value < kMinBrightness) || std::isnan(value)) {
    return ZX_ERR_INVALID_ARGS;
  }

  aml_pwm::mode_config regular = {aml_pwm::Mode::kOn, {}};
  fuchsia_hardware_pwm::wire::PwmConfig cfg = {
      .polarity = false,
      .period_ns = static_cast<uint32_t>(pwm_period_.to_nsecs()),
      .duty_cycle = static_cast<float>(value * 100.0 / (kMaxBrightness * 1.0)),
      .mode_config = fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&regular),
                                                             sizeof(regular)),
  };
  auto result = (*pwm_)->SetConfig(cfg);
  if (!result.ok()) {
    zxlogf(ERROR, "PWM set config failed: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "PWM set config failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  value_ = value;
  return ZX_OK;
}

void AmlLight::GetNumLights(GetNumLightsCompleter::Sync& completer) {
  completer.Reply(static_cast<uint32_t>(lights_.size()));
}

void AmlLight::GetNumLightGroups(GetNumLightGroupsCompleter::Sync& completer) {
  completer.Reply(0);
}

void AmlLight::GetInfo(GetInfoRequestView request, GetInfoCompleter::Sync& completer) {
  if (request->index >= lights_.size()) {
    completer.ReplyError(LightError::kInvalidIndex);
    return;
  }
  auto name = lights_[request->index].GetName();
  completer.ReplySuccess({
      .name = ::fidl::StringView::FromExternal(name),
      .capability = lights_[request->index].GetCapability(),
  });
}

void AmlLight::GetCurrentSimpleValue(GetCurrentSimpleValueRequestView request,
                                     GetCurrentSimpleValueCompleter::Sync& completer) {
  if (request->index >= lights_.size()) {
    completer.ReplyError(LightError::kInvalidIndex);
    return;
  }
  if (lights_[request->index].GetCapability() == Capability::kSimple) {
    completer.ReplySuccess(lights_[request->index].GetCurrentSimpleValue());
  } else {
    completer.ReplyError(LightError::kNotSupported);
  }
}

void AmlLight::SetSimpleValue(SetSimpleValueRequestView request,
                              SetSimpleValueCompleter::Sync& completer) {
  if (request->index >= lights_.size()) {
    completer.ReplyError(LightError::kInvalidIndex);
    return;
  }
  if (lights_[request->index].SetSimpleValue(request->value) != ZX_OK) {
    completer.ReplyError(LightError::kFailed);
  } else {
    completer.ReplySuccess();
  }
}

void AmlLight::GetCurrentBrightnessValue(GetCurrentBrightnessValueRequestView request,
                                         GetCurrentBrightnessValueCompleter::Sync& completer) {
  if (request->index >= lights_.size()) {
    completer.ReplyError(LightError::kInvalidIndex);
    return;
  }
  if (lights_[request->index].GetCapability() == Capability::kBrightness) {
    completer.ReplySuccess(lights_[request->index].GetCurrentBrightnessValue());
  } else {
    completer.ReplyError(LightError::kNotSupported);
  }
}

void AmlLight::SetBrightnessValue(SetBrightnessValueRequestView request,
                                  SetBrightnessValueCompleter::Sync& completer) {
  if (request->index >= lights_.size()) {
    completer.ReplyError(LightError::kInvalidIndex);
    return;
  }
  if (lights_[request->index].SetBrightnessValue(request->value) != ZX_OK) {
    completer.ReplyError(LightError::kFailed);
  } else {
    completer.ReplySuccess();
  }
}

void AmlLight::GetCurrentRgbValue(GetCurrentRgbValueRequestView request,
                                  GetCurrentRgbValueCompleter::Sync& completer) {
  completer.ReplyError(LightError::kNotSupported);
}

void AmlLight::SetRgbValue(SetRgbValueRequestView request, SetRgbValueCompleter::Sync& completer) {
  completer.ReplyError(LightError::kInvalidIndex);
}

void AmlLight::DdkRelease() { delete this; }

zx_status_t AmlLight::Create(void* ctx, zx_device_t* parent) {
  fbl::AllocChecker ac;
  auto dev = std::unique_ptr<AmlLight>(new (&ac) AmlLight(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto status = dev->Init();
  if (status != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = dev.release();
  return ZX_OK;
}

zx_status_t AmlLight::Init() {
  zx_status_t status = ZX_OK;
  struct name_t {
    char name[kNameLength];
  };
  auto names = ddk::GetMetadataArray<name_t>(parent(), DEVICE_METADATA_NAME);
  if (!names.is_ok()) {
    return names.error_value();
  }
  auto configs = ddk::GetMetadataArray<lights_config_t>(parent(), DEVICE_METADATA_LIGHTS);
  if (!configs.is_ok()) {
    return configs.error_value();
  }
  if (names->size() != configs->size()) {
    zxlogf(ERROR, "number of names [%lu] does not match number of configs [%lu]", names->size(),
           configs->size());
    return ZX_ERR_INTERNAL;
  }

  ddk::PDevFidl pdev(parent(), "pdev");
  pdev_board_info_t board_info = {};
  status = ZX_OK;
  if (!pdev.is_valid() || (status = pdev.GetBoardInfo(&board_info)) != ZX_OK) {
    board_info.pid = PDEV_PID_GENERIC;
  }

  const zx::duration pwm_period = board_info.pid == PDEV_PID_NELSON ? kNelsonPwmPeriod : kPwmPeriod;

  zxlogf(INFO, "PWM period: %ld ns", pwm_period.to_nsecs());

  for (uint32_t i = 0; i < configs->size(); i++) {
    auto* config = &configs.value()[i];
    char* name = names.value()[i].name;

    std::string fragment_name;
    if (std::string("AMBER_LED") == name) {
      fragment_name = "amber-led";
    } else if (std::string("GREEN_LED") == name) {
      fragment_name = "green-led";
    } else {
      zxlogf(ERROR, "Unsupported light: %s", name);
      return ZX_ERR_NOT_SUPPORTED;
    }

    zx::result gpio = DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
        parent(), ("gpio-" + fragment_name).c_str());
    if (gpio.is_error()) {
      zxlogf(ERROR, "Failed to get gpio protocol from fragment gpio-%s: %s", fragment_name.c_str(),
             gpio.status_string());
      return gpio.status_value();
    }

    if (config->brightness) {
      zx::result client_end = DdkConnectFragmentFidlProtocol<fuchsia_hardware_pwm::Service::Pwm>(
          parent(), ("pwm-" + fragment_name).c_str());
      if (client_end.is_error()) {
        zxlogf(ERROR, "Failed to initialize PWM Client, st = %s", client_end.status_string());
        return client_end.status_value();
      }
      fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm(std::move(client_end.value()));

      lights_.emplace_back(name, std::move(gpio.value()), std::move(pwm), pwm_period);
    } else {
      lights_.emplace_back(name, std::move(gpio.value()), std::nullopt, pwm_period);
    }

    if ((status = lights_.back().Init(config->init_on)) != ZX_OK) {
      zxlogf(ERROR, "Could not initialize light");
      return status;
    }

    // RGB not supported, so not implemented.
  }

  return DdkAdd("gpio-light", DEVICE_ADD_NON_BINDABLE);
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlLight::Create;
  return ops;
}();

}  // namespace aml_light

ZIRCON_DRIVER(aml_light, aml_light::driver_ops, "zircon", "0.1");

// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-pwm-init.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <unistd.h>

#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace pwm_init {

const char* kWifiClkFragName = "wifi-32k768-clk";

zx_status_t PwmInitDevice::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;

  zx::result client_end =
      DdkConnectFragmentFidlProtocol<fuchsia_hardware_pwm::Service::Pwm>(parent, "pwm");
  if (client_end.is_error()) {
    zxlogf(ERROR, "Failed to initialize PWM Client, st = %s", client_end.status_string());
    return client_end.status_value();
  }
  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm(std::move(client_end.value()));
  ddk::GpioProtocolClient wifi_gpio(parent, "gpio-wifi");
  ddk::GpioProtocolClient bt_gpio(parent, "gpio-bt");
  if (!pwm.is_valid() || !wifi_gpio.is_valid() || !bt_gpio.is_valid()) {
    zxlogf(ERROR, "could not get fragments");
    return ZX_ERR_NO_RESOURCES;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<PwmInitDevice> dev(new (&ac)
                                         PwmInitDevice(parent, std::move(pwm), wifi_gpio, bt_gpio));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if ((status = dev->Init()) != ZX_OK) {
    zxlogf(ERROR, "could not initialize PWM for bluetooth and SDIO");
    return status;
  }

  zx_device_prop_t props[] = {
      {BIND_INIT_STEP, 0, bind_fuchsia_hardware_pwm::BIND_INIT_STEP_PWM},
  };
  status = dev->DdkAdd(ddk::DeviceAddArgs("aml-pwm-init")
                           .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                           .set_props(props));
  if (status != ZX_OK) {
    return status;
  }

  // dev is now owned by devmgr.
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

zx_status_t PwmInitDevice::Init() {
  zx_status_t status = ZX_OK;

  // Configure SOC_WIFI_LPO_32k768 pin for PWM_E
  if (((status = wifi_gpio_.SetAltFunction(1)) != ZX_OK)) {
    zxlogf(ERROR, "could not initialize GPIO for WIFI");
    return ZX_ERR_NO_RESOURCES;
  }

  // Enable PWM_CLK_* for WIFI 32K768
  zx::result clock_result =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_clock::Service::Clock>(
          parent(), kWifiClkFragName);
  if (clock_result.is_ok()) {
    fidl::WireSyncClient<fuchsia_hardware_clock::Clock> wifi_32k768_clk{std::move(*clock_result)};
    fidl::WireResult result = wifi_32k768_clk->Enable();
    if (!result.ok()) {
      zxlogf(WARNING, "Failed to send Enable request to clock for wifi_32k768: %s",
             result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(WARNING, "Failed to enable clock for wifi_32k768: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  auto result = pwm_->Enable();
  if (!result.ok()) {
    zxlogf(ERROR, "Could not enable PWM: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Could not enable PWM: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  aml_pwm::mode_config two_timer = {
      .mode = aml_pwm::Mode::kTwoTimer,
      .two_timer =
          {
              .period_ns2 = 30052,
              .duty_cycle2 = 50.0,
              .timer1 = 0x0a,
              .timer2 = 0x0a,
          },
  };
  fuchsia_hardware_pwm::wire::PwmConfig init_cfg = {
      .polarity = false,
      .period_ns = 30053,
      .duty_cycle = static_cast<float>(49.931787176),
      .mode_config = fidl::VectorView<uint8_t>::FromExternal(reinterpret_cast<uint8_t*>(&two_timer),
                                                             sizeof(two_timer)),
  };
  auto set_config_result = pwm_->SetConfig(init_cfg);
  if (!set_config_result.ok()) {
    zxlogf(ERROR, "Could not initialize PWM: %s", set_config_result.status_string());
    return set_config_result.status();
  }
  if (set_config_result->is_error()) {
    zxlogf(ERROR, "Could not initialize PWM: %s",
           zx_status_get_string(set_config_result->error_value()));
    return set_config_result->error_value();
  }

  // set GPIO to reset Bluetooth module
  if ((status = bt_gpio_.ConfigOut(0)) != ZX_OK) {
    zxlogf(ERROR, "Could not initialize GPIO for Bluetooth");
    return status;
  }
  usleep(10 * 1000);
  if ((status = bt_gpio_.Write(1)) != ZX_OK) {
    zxlogf(ERROR, "Could not initialize GPIO for Bluetooth");
    return status;
  }
  usleep(100 * 1000);

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = PwmInitDevice::Create;
  return ops;
}();

}  // namespace pwm_init

ZIRCON_DRIVER(pwm_init, pwm_init::driver_ops, "zircon", "0.1");

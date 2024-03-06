// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-pwm-init.h"

#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <unistd.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace pwm_init {

const char* kWifiClkFragName = "wifi-32k768-clk";
const char* kDeviceName = "aml-pwm-init";

PwmInitDriver::PwmInitDriver(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher dispatcher)
    : fdf::DriverBase(kDeviceName, std::move(start_args), std::move(dispatcher)) {}

zx::result<> PwmInitDriver::Start() {
  zx_status_t status;
  zx::result init_result = compat_server_.Initialize(incoming(), outgoing(), node_name(),
                                                     kDeviceName, compat::ForwardMetadata::All());
  if (init_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize compat server, st = %s", init_result.status_string());
    return init_result.take_error();
  }

  zx::result clock_result =
      incoming()->Connect<fuchsia_hardware_clock::Service::Clock>(kWifiClkFragName);
  if (clock_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize Clock Client, st = %s", clock_result.status_string());
    return clock_result.take_error();
  }

  zx::result client_end = incoming()->Connect<fuchsia_hardware_pwm::Service::Pwm>("pwm");
  if (client_end.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize PWM Client, st = %s", client_end.status_string());
    return client_end.take_error();
  }
  fidl::WireSyncClient<fuchsia_hardware_pwm::Pwm> pwm(std::move(client_end.value()));

  const char* kWifiGpioFragmentName = "gpio-wifi";
  zx::result wifi_gpio =
      incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(kWifiGpioFragmentName);
  if (wifi_gpio.is_error()) {
    FDF_LOG(ERROR, "Failed to get gpio FIDL protocol from fragment %s: %s", kWifiGpioFragmentName,
            wifi_gpio.status_string());
    return wifi_gpio.take_error();
  }

  const char* kBtGpioFragmentName = "gpio-bt";
  zx::result bt_gpio =
      incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(kBtGpioFragmentName);
  if (bt_gpio.is_error()) {
    FDF_LOG(ERROR, "Failed to get gpio FIDL protocol from fragment %s: %s", kBtGpioFragmentName,
            bt_gpio.status_string());
    return bt_gpio.take_error();
  }

  initer_ =
      std::make_unique<PwmInitDevice>(std::move(clock_result.value()), std::move(pwm),
                                      std::move(wifi_gpio.value()), std::move(bt_gpio.value()));

  if ((status = initer_->Init()) != ZX_OK) {
    FDF_LOG(ERROR, "could not initialize PWM for bluetooth and SDIO. st = %s",
            zx_status_get_string(status));
    return zx::error(status);
  }

  node_client_.Bind(std::move(node()));

  fidl::Arena arena;
  auto properties = std::vector{
      fdf::MakeProperty(arena, bind_fuchsia::INIT_STEP, bind_fuchsia_pwm::BIND_INIT_STEP_PWM)};

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return controller_endpoints.take_error();
  }

  controller_.Bind(std::move(controller_endpoints->client));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers2(compat_server_.CreateOffers2(arena))
                        .properties(arena, std::move(properties))
                        .Build();
  fidl::WireResult result =
      node_client_->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send request to add child: %s", result.status_string());
    return zx::error(result.status());
  }
  return zx::ok();
}

zx_status_t PwmInitDevice::Init() {
  // Configure SOC_WIFI_LPO_32k768 pin for PWM_E
  {
    fidl::WireResult result = wifi_gpio_->SetAltFunction(1);
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to send SetAltFunction request to wifi gpio: %s",
              result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "Failed to set wifi gpio's alt function: %s",
              zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Enable PWM_CLK_* for WIFI 32K768
  // This connection is optional, so if it does not connect, don't return an error.
  // In DFv2 Connect will succeed even if the fragment is not there, so we first learn of the
  // failed connection when sending the Enable command.
  {
    fidl::WireResult result = wifi_32k768_clk_->Enable();
    if (!result.ok()) {
      FDF_LOG(WARNING, "Failed to send Enable request to clock for wifi_32k768: %s",
              result.status_string());
    } else {  // only check the returned result if we actually got a valid response.
      if (result->is_error()) {
        FDF_LOG(WARNING, "Failed to enable clock for wifi_32k768: %s",
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }

  auto result = pwm_->Enable();
  if (!result.ok()) {
    FDF_LOG(ERROR, "Could not enable PWM: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    FDF_LOG(ERROR, "Could not enable PWM: %s", zx_status_get_string(result->error_value()));
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
    FDF_LOG(ERROR, "Could not initialize PWM: %s", set_config_result.status_string());
    return set_config_result.status();
  }
  if (set_config_result->is_error()) {
    FDF_LOG(ERROR, "Could not initialize PWM: %s",
            zx_status_get_string(set_config_result->error_value()));
    return set_config_result->error_value();
  }

  // set GPIO to reset Bluetooth module
  fidl::WireResult config_result = bt_gpio_->ConfigOut(0);
  if (!config_result.ok()) {
    FDF_LOG(ERROR, "Failed to send ConfigOut request to bt gpio: %s",
            config_result.status_string());
    return config_result.status();
  }
  if (config_result->is_error()) {
    FDF_LOG(ERROR, "Failed to configure bt gpio to output: %s",
            zx_status_get_string(config_result->error_value()));
    return config_result->error_value();
  }
  usleep(10 * 1000);
  fidl::WireResult write_result = bt_gpio_->Write(1);
  if (!write_result.ok()) {
    FDF_LOG(ERROR, "Failed to send Write request to bt gpio: %s", write_result.status_string());
    return write_result.status();
  }
  if (write_result->is_error()) {
    FDF_LOG(ERROR, "Failed to write to bt gpio: %s",
            zx_status_get_string(write_result->error_value()));
    return write_result->error_value();
  }
  usleep(100 * 1000);

  return ZX_OK;
}

}  // namespace pwm_init

FUCHSIA_DRIVER_EXPORT(pwm_init::PwmInitDriver);

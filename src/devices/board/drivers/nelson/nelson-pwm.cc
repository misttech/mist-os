// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.pwm/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <soc/aml-s905d3/s905d3-pwm.h>

#include "nelson-gpios.h"
#include "nelson.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> pwm_mmios{
    {{
        .base = S905D3_PWM_AB_BASE,
        .length = S905D3_PWM_AB_LENGTH,
    }},
    {{
        .base = S905D3_PWM_CD_BASE,
        .length = S905D3_PWM_AB_LENGTH,
    }},
    {{
        .base = S905D3_PWM_EF_BASE,
        .length = S905D3_PWM_AB_LENGTH,
    }},
    {{
        .base = S905D3_AO_PWM_AB_BASE,
        .length = S905D3_AO_PWM_LENGTH,
    }},
    {{
        .base = S905D3_AO_PWM_CD_BASE,
        .length = S905D3_AO_PWM_LENGTH,
    }},
};

static fpbus::Node pwm_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "pwm";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_S905D3;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_PWM;
  dev.mmio() = pwm_mmios;
  return dev;
}();

const ddk::BindRule kPwmRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_pwm::SERVICE,
                            bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::PWM_ID, static_cast<uint32_t>(S905D3_PWM_E)),
};

const device_bind_prop_t kPwmProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_pwm::SERVICE,
                      bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
};

const ddk::BindRule kGpioWifiRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN,
                            static_cast<uint32_t>(GPIO_SOC_WIFI_LPO_32K768)),
};

const device_bind_prop_t kGpioWifiProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_WIFI_LPO),
};

const ddk::BindRule kGpioBtRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_SOC_BT_REG_ON)),
};

const device_bind_prop_t kGpioBtProperties[] = {
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    ddk::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_BT_REG_ON),
};

zx_status_t Nelson::PwmInit() {
  fuchsia_hardware_pwm::PwmChannelsMetadata metadata = {{{{
      {{.id = S905D3_PWM_A}},
      {{.id = S905D3_PWM_B}},
      {{.id = S905D3_PWM_C}},
      {{.id = S905D3_PWM_D}},
      {{.id = S905D3_PWM_E}},
      {{.id = S905D3_PWM_F}},
      {{.id = S905D3_PWM_AO_A}},
      {{.id = S905D3_PWM_AO_B, .skip_init = true}},
      {{.id = S905D3_PWM_AO_C}},
      {{.id = S905D3_PWM_AO_D, .skip_init = true}},
  }}}};

  fit::result encoded_metadata = fidl::Persist(metadata);
  if (encoded_metadata.is_error()) {
    zxlogf(ERROR, "Failed to encode pwm channels metadata: %s",
           encoded_metadata.error_value().FormatDescription().c_str());
    return encoded_metadata.error_value().status();
  }

  const std::vector<fpbus::Metadata> pwm_metadata{
      {{
          .type = DEVICE_METADATA_PWM_CHANNELS,
          .data = encoded_metadata.value(),
      }},
  };
  pwm_dev.metadata() = pwm_metadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('PWM_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, pwm_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Pwm(pwm_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Pwm(pwm_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  zx_status_t status =
      DdkAddCompositeNodeSpec("pwm_init", ddk::CompositeNodeSpec(kPwmRules, kPwmProperties)
                                              .AddParentSpec(kGpioWifiRules, kGpioWifiProperties)
                                              .AddParentSpec(kGpioBtRules, kGpioBtProperties));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }

  zxlogf(INFO, "Added PwmInitDevice");

  return ZX_OK;
}

}  // namespace nelson

// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <ddk/metadata/power.h>
#include <soc/aml-common/aml-power.h>
#include <soc/aml-s905d2/s905d2-power.h>
#include <soc/aml-s905d2/s905d2-pwm.h>

#include "astro-gpios.h"
#include "astro.h"
#include "src/devices/board/drivers/astro/pd-armcore-bind.h"
#include "src/devices/board/drivers/astro/pwm-ao-d-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

constexpr aml_voltage_table_t kS905D2VoltageTable[] = {
    {1'022'000, 0}, {1'011'000, 3}, {1'001'000, 6}, {991'000, 10}, {981'000, 13}, {971'000, 16},
    {961'000, 20},  {951'000, 23},  {941'000, 26},  {931'000, 30}, {921'000, 33}, {911'000, 36},
    {901'000, 40},  {891'000, 43},  {881'000, 46},  {871'000, 50}, {861'000, 53}, {851'000, 56},
    {841'000, 60},  {831'000, 63},  {821'000, 67},  {811'000, 70}, {801'000, 73}, {791'000, 76},
    {781'000, 80},  {771'000, 83},  {761'000, 86},  {751'000, 90}, {741'000, 93}, {731'000, 96},
    {721'000, 100},
};

constexpr voltage_pwm_period_ns_t kS905d2PwmPeriodNs = 1250;

constexpr power_domain_t domains[] = {
    {static_cast<uint32_t>(S905d2PowerDomains::kArmCore)},
};

}  // namespace

zx_status_t AddPowerImpl(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node dev;
  dev.name() = "aml-power-impl-composite";
  dev.vid() = PDEV_VID_GOOGLE;
  dev.pid() = PDEV_PID_ASTRO;
  dev.did() = PDEV_DID_AMLOGIC_POWER;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_AML_VOLTAGE_TABLE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kS905D2VoltageTable),
              reinterpret_cast<const uint8_t*>(&kS905D2VoltageTable) + sizeof(kS905D2VoltageTable)),
      }},
      {{
          .type = DEVICE_METADATA_AML_PWM_PERIOD_NS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kS905d2PwmPeriodNs),
              reinterpret_cast<const uint8_t*>(&kS905d2PwmPeriodNs) + sizeof(kS905d2PwmPeriodNs)),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('POWR');
  auto fragments = platform_bus_composite::MakeFidlFragment(fidl_arena, aml_power_impl_fragments,
                                                            std::size(aml_power_impl_fragments));
  fdf::WireUnownedResult result =
      pbus.buffer(arena)->AddComposite(fidl::ToWire(fidl_arena, dev), fragments, "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add composite: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t AddPdArmcore(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus) {
  fpbus::Node dev;
  dev.name() = "composite-pd-armcore";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_POWER_CORE;
  dev.metadata() = std::vector<fpbus::Metadata>{
      {{
          .type = DEVICE_METADATA_POWER_DOMAINS,
          .data =
              std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&domains),
                                   reinterpret_cast<const uint8_t*>(&domains) + sizeof(domains)),
      }},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('PDAC');
  auto fragments = platform_bus_composite::MakeFidlFragment(
      fidl_arena, power_domain_arm_core_fragments, std::size(power_domain_arm_core_fragments));
  fdf::WireUnownedResult result =
      pbus.buffer(arena)->AddComposite(fidl::ToWire(fidl_arena, dev), fragments, "power-impl");
  if (!result.ok()) {
    zxlogf(ERROR, "Failed to send AddComposite request: %s", result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "Failed to add composite: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

zx_status_t Astro::PowerInit() {
  zx_status_t status = AddPowerImpl(pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add power-impl composite device: %s", zx_status_get_string(status));
    return status;
  }

  status = AddPdArmcore(pbus_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add pd-armcore composite device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace astro

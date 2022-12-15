// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>

#include <soc/aml-a1/a1-hw.h>
#include <soc/aml-common/aml-cpu-metadata.h>

#include "clover.h"
#include "src/devices/board/drivers/clover/clover-cpu-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

static constexpr amlogic_cpu::PerfDomainId kPdArmA35 = 1;

static const std::vector<fpbus::Mmio> cpu_mmios{
    {{
        .base = A1_SYS_CTRL_BASE,
        .length = A1_SYS_CTRL_LENGTH,
    }},
};

static constexpr amlogic_cpu::operating_point_t operating_points[] = {
    {.freq_hz = 128'000'000, .volt_uv = 800'000, .pd_id = kPdArmA35},
    {.freq_hz = 256'000'000, .volt_uv = 800'000, .pd_id = kPdArmA35},
    {.freq_hz = 512'000'000, .volt_uv = 800'000, .pd_id = kPdArmA35},
    {.freq_hz = 768'000'000, .volt_uv = 800'000, .pd_id = kPdArmA35},
    {.freq_hz = 1'008'000'000, .volt_uv = 800'000, .pd_id = kPdArmA35},
    {.freq_hz = 1'200'000'000, .volt_uv = 800'000, .pd_id = kPdArmA35},
};

static constexpr amlogic_cpu::perf_domain_t performance_domains[] = {
    {.id = kPdArmA35, .core_count = 2, .relative_performance = 255, .name = "a1-arm-a35"},
};

static const std::vector<fpbus::Metadata> cpu_metadata{
    {{
        .type = DEVICE_METADATA_AML_OP_POINTS,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&operating_points),
            reinterpret_cast<const uint8_t*>(&operating_points) + sizeof(operating_points)),
    }},
    {{
        .type = DEVICE_METADATA_AML_PERF_DOMAINS,
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&performance_domains),
            reinterpret_cast<const uint8_t*>(&performance_domains) + sizeof(performance_domains)),
    }},
};

static const fpbus::Node cpu_dev = []() {
  fpbus::Node result = {};
  result.name() = "aml-cpu";
  result.vid() = PDEV_VID_AMLOGIC;
  result.pid() = PDEV_PID_AMLOGIC_A1;
  result.did() = PDEV_DID_AMLOGIC_CPU;
  result.metadata() = cpu_metadata;
  result.mmio() = cpu_mmios;

  return result;
}();

}  // namespace

namespace clover {

zx_status_t Clover::CpuInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('CPU_');
  auto composite_result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, cpu_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, aml_cpu_fragments,
                                               std::size(aml_cpu_fragments)),
      fidl::StringView::FromExternal("pdev"));

  if (!composite_result.ok()) {
    zxlogf(ERROR, "AddComposite request failed: %s", composite_result.FormatDescription().data());
    return composite_result.status();
  }
  if (composite_result->is_error()) {
    zxlogf(ERROR, "AddComposite failed: %s", zx_status_get_string(composite_result->error_value()));
    return composite_result->error_value();
  }

  return ZX_OK;
}

}  // namespace clover

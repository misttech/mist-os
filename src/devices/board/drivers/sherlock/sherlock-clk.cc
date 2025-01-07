// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.clockimpl/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <ddk/metadata/clock.h>
#include <soc/aml-meson/g12b-clk.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> clk_mmios{
    {{
        .base = T931_HIU_BASE,
        .length = T931_HIU_LENGTH,
    }},
    {{
        .base = T931_DOS_BASE,
        .length = T931_DOS_LENGTH,
    }},
    {{
        .base = T931_MSR_CLK_BASE,
        .length = T931_MSR_CLK_LENGTH,
    }},
};

zx_status_t Sherlock::ClkInit() {
  fuchsia_hardware_clockimpl::wire::InitMetadata clock_init_metadata;
  clock_init_metadata.steps =
      fidl::VectorView<fuchsia_hardware_clockimpl::wire::InitStep>::FromExternal(
          clock_init_steps_.data(), clock_init_steps_.size());

  fit::result encoded_clock_init_metadata = fidl::Persist(clock_init_metadata);
  if (!encoded_clock_init_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode clock init metadata: %s",
           encoded_clock_init_metadata.error_value().FormatDescription().c_str());
    return encoded_clock_init_metadata.error_value().status();
  }

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
  const fuchsia_hardware_clockimpl::ClockIdsMetadata kClockIdsMetadata{{
      .clock_ids{{
          // For Camera Sensor.
          g12b_clk::G12B_CLK_CAM_INCK_24M,

          // For cpu driver.
          g12b_clk::G12B_CLK_SYS_PLL_DIV16,
          g12b_clk::G12B_CLK_SYS_CPU_CLK_DIV16,
          g12b_clk::G12B_CLK_SYS_PLLB_DIV16,
          g12b_clk::G12B_CLK_SYS_CPUB_CLK_DIV16,
          g12b_clk::CLK_SYS_CPU_BIG_CLK,
          g12b_clk::CLK_SYS_CPU_LITTLE_CLK,

          // For video decoder/encoder
          g12b_clk::G12B_CLK_DOS_GCLK_VDEC,
          g12b_clk::G12B_CLK_DOS_GCLK_HCODEC,
          g12b_clk::G12B_CLK_DOS,
      }},
  }};
  const fit::result encoded_clock_ids_metadata = fidl::Persist(kClockIdsMetadata);
  if (!encoded_clock_ids_metadata.is_ok()) {
    zxlogf(ERROR, "Failed to encode clock ID's: %s",
           encoded_clock_ids_metadata.error_value().FormatDescription().c_str());
    return encoded_clock_ids_metadata.error_value().status();
  }
#endif

  const std::vector<fpbus::Metadata> clock_metadata{
#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)
      {{
          .id = fuchsia_hardware_clockimpl::ClockIdsMetadata::kSerializableName,
          .data = encoded_clock_ids_metadata.value(),
      }},
#endif
      // TODO(b/373903133): Remove once no longer referenced.
      {{
          .id = std::to_string(DEVICE_METADATA_CLOCK_INIT),
          .data = encoded_clock_init_metadata.value(),
      }},
      {{
          .id = fuchsia_hardware_clockimpl::wire::InitMetadata::kSerializableName,
          .data = std::move(encoded_clock_init_metadata.value()),
      }},
  };

  const fpbus::Node clk_dev = [&clock_metadata]() {
    fpbus::Node dev = {};
    dev.name() = "sherlock-clk";
    dev.vid() = PDEV_VID_AMLOGIC;
    dev.did() = PDEV_DID_AMLOGIC_G12B_CLK;
    dev.mmio() = clk_mmios;
    dev.metadata() = clock_metadata;
    return dev;
  }();

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('CLK_');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, clk_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Clk(clk_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Clk(clk_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  clock_init_steps_.clear();

  return ZX_OK;
}

}  // namespace sherlock

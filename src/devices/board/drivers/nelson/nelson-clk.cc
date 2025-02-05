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
#include <soc/aml-meson/sm1-clk.h>
#include <soc/aml-s905d3/s905d3-hw.h>

#include "nelson.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> clk_mmios{
    {{
        .base = S905D3_HIU_BASE,
        .length = S905D3_HIU_LENGTH,
    }},
    {{
        .base = S905D3_DOS_BASE,
        .length = S905D3_DOS_LENGTH,
    }},
    {{
        .base = S905D3_MSR_CLK_BASE,
        .length = S905D3_MSR_CLK_LENGTH,
    }},
};

// TODO(b/373903133): Remove once no longer referenced.
constexpr clock_id_t kClockIds[] = {
    {sm1_clk::CLK_RESET},  // PLACEHOLDER.

    // For audio driver.
    {sm1_clk::CLK_HIFI_PLL},
    {sm1_clk::CLK_SYS_PLL_DIV16},
    {sm1_clk::CLK_SYS_CPU_CLK_DIV16},

    // For video decoder
    {sm1_clk::CLK_DOS_GCLK_VDEC},
    {sm1_clk::CLK_DOS},

    // For GPU
    {sm1_clk::CLK_GP0_PLL},
};

zx_status_t Nelson::ClkInit() {
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
          sm1_clk::CLK_RESET,  // PLACEHOLDER.

          // For audio driver.
          sm1_clk::CLK_HIFI_PLL,
          sm1_clk::CLK_SYS_PLL_DIV16,
          sm1_clk::CLK_SYS_CPU_CLK_DIV16,

          // For video decoder
          sm1_clk::CLK_DOS_GCLK_VDEC,
          sm1_clk::CLK_DOS,

          // For GPU
          sm1_clk::CLK_GP0_PLL,
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
          .id = fuchsia_hardware_clockimpl::wire::ClockIdsMetadata::kSerializableName,
          .data = encoded_clock_ids_metadata.value(),
      }},
#endif
      // TODO(b/373903133): Remove once no longer referenced.
      {{
          .id = std::to_string(DEVICE_METADATA_CLOCK_IDS),
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kClockIds),
              reinterpret_cast<const uint8_t*>(&kClockIds) + sizeof(kClockIds)),
      }},
      {{
          .id = fuchsia_hardware_clockimpl::wire::InitMetadata::kSerializableName,
          .data = std::move(encoded_clock_init_metadata.value()),
      }},
  };

  const fpbus::Node clk_dev = [&clock_metadata]() {
    fpbus::Node dev = {};
    dev.name() = "nelson-clk";
    dev.vid() = PDEV_VID_AMLOGIC;
    dev.pid() = PDEV_PID_AMLOGIC_S905D3;
    dev.did() = PDEV_DID_AMLOGIC_SM1_CLK;
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

  return ZX_OK;
}

}  // namespace nelson

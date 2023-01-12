// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <zircon/syscalls/smc.h>

#include <soc/aml-a1/a1-hw.h>

#include "clover.h"
#include "src/devices/board/drivers/clover/clover-dsp-bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace clover {
namespace fpbus = fuchsia_hardware_platform_bus;

// Clover's DSPA image load memory space is defined within the bootloader image.
#define A1_DSPA_DDR_BASE 0x3400000
#define A1_DSPA_DDR_BASE_LENGTH 0x300000

static const std::vector<fpbus::Mmio> dsp_mmios{
    {{
        .base = A1_DSPA_BASE,
        .length = A1_DSPA_BASE_LENGTH,
    }},
    {{
        .base = A1_DSPA_DDR_BASE,
        .length = A1_DSPA_DDR_BASE_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> dsp_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_DSP,
    }},
};

static const std::vector<fpbus::Smc> dsp_smcs{
    {{
        .service_call_num_base = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_BASE,
        .count = ARM_SMC_SERVICE_CALL_NUM_SIP_SERVICE_LENGTH,
        .exclusive = false,
    }},
};

static const fpbus::Node dsp_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "dsp";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_AMLOGIC_A1;
  dev.did() = PDEV_DID_AMLOGIC_DSP;
  dev.mmio() = dsp_mmios;
  dev.smc() = dsp_smcs;
  dev.bti() = dsp_btis;
  return dev;
}();

zx_status_t Clover::DspInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DSP_');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, dsp_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, clover_dsp_fragments,
                                               std::size(clover_dsp_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "AddComposite Dsp(dsp_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddComposite Dsp(dsp_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace clover

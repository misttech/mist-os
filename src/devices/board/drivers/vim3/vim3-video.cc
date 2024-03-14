// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/amlogic/platform/meson/cpp/bind.h>
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <bind/fuchsia/hardware/sysmem/cpp/bind.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-meson/g12b-clk.h>

#include "src/devices/board/drivers/vim3/vim3.h"

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> vim_video_mmios{
    {{
        .base = A311D_FULL_CBUS_BASE,
        .length = A311D_FULL_CBUS_LENGTH,
    }},
    {{
        .base = A311D_DOS_BASE,
        .length = A311D_DOS_LENGTH,
    }},
    {{
        .base = A311D_HIU_BASE,
        .length = A311D_HIU_LENGTH,
    }},
    {{
        .base = A311D_AOBUS_BASE,
        .length = A311D_AOBUS_LENGTH,
    }},
    {{
        .base = A311D_DMC_BASE,
        .length = A311D_DMC_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> vim_video_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_VIDEO,
    }},
};

static const std::vector<fpbus::Irq> vim_video_irqs{
    {{
        .irq = A311D_DEMUX_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_PARSER_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_DOS_MBOX_0_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
    {{
        .irq = A311D_DOS_MBOX_1_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

zx_status_t Vim3::VideoInit() {
  fpbus::Node video_dev;
  video_dev.name() = "aml_video";
  video_dev.vid() = PDEV_VID_AMLOGIC;
  video_dev.pid() = PDEV_PID_AMLOGIC_A311D;
  video_dev.did() = PDEV_DID_AMLOGIC_VIDEO;
  video_dev.mmio() = vim_video_mmios;
  video_dev.irq() = vim_video_irqs;
  video_dev.bti() = vim_video_btis;

  auto video_sysmem = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_sysmem::SERVICE,
                                      bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_sysmem::SERVICE,
                                bind_fuchsia_hardware_sysmem::SERVICE_ZIRCONTRANSPORT),
          },
  }};

  auto video_canvas = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                                      bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                                bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
          },
  }};

  auto video_clock_dos_vdec = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                      bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(
                  bind_fuchsia::CLOCK_ID,
                  bind_fuchsia_amlogic_platform_meson::G12B_CLK_ID_CLK_DOS_GCLK_VDEC),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                                bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia::CLOCK_ID, bind_fuchsia_clock::FUNCTION_DOS_GCLK_VDEC),
          },
  }};

  auto video_clock_dos = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_clock::SERVICE,
                                      bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia::CLOCK_ID,
                                      bind_fuchsia_amlogic_platform_meson::G12B_CLK_ID_CLK_DOS),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_clock::SERVICE,
                                bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia::CLOCK_ID, bind_fuchsia_clock::FUNCTION_DOS),
          },
  }};

  auto video_spec = fuchsia_driver_framework::CompositeNodeSpec{{
      .name = "aml_video",
      .parents = {{video_sysmem, video_canvas, video_clock_dos_vdec, video_clock_dos}},
  }};

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('VIDE');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, video_dev),
                                                          fidl::ToWire(fidl_arena, video_spec));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Video(video_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Video(video_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}
}  // namespace vim3

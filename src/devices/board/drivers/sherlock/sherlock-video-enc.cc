// Copyright 2020 The Fuchsia Authors. All rights reserved.
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
#include <bind/fuchsia/clock/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/amlogiccanvas/cpp/bind.h>
#include <bind/fuchsia/hardware/clock/cpp/bind.h>
#include <soc/aml-meson/g12b-clk.h>
#include <soc/aml-t931/t931-hw.h>

#include "sherlock.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> sherlock_video_enc_mmios{
    {{
        .base = T931_CBUS_BASE,
        .length = T931_CBUS_LENGTH,
    }},
    {{
        .base = T931_DOS_BASE,
        .length = T931_DOS_LENGTH,
    }},
    {{
        .base = T931_AOBUS_BASE,
        .length = T931_AOBUS_LENGTH,
    }},
    {{
        .base = T931_HIU_BASE,
        .length = T931_HIU_LENGTH,
    }},
};

static const std::vector<fpbus::Bti> sherlock_video_enc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_VIDEO_ENC,
    }},
};

static const std::vector<fpbus::Irq> sherlock_video_enc_irqs{
    {{
        .irq = T931_DOS_MBOX_2_IRQ,
        .mode = fpbus::ZirconInterruptMode::kEdgeHigh,
    }},
};

const std::vector<fdf::BindRule2> kCanvasRules = std::vector{
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                             bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT)};

const std::vector<fdf::NodeProperty2> kCanvasProperties = std::vector{
    fdf::MakeProperty2(bind_fuchsia_hardware_amlogiccanvas::SERVICE,
                       bind_fuchsia_hardware_amlogiccanvas::SERVICE_ZIRCONTRANSPORT),
};

const std::vector<fdf::BindRule2> kClkDosHCodecRules = std::vector{
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_clock::SERVICE,
                             bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule2(bind_fuchsia::CLOCK_ID, g12b_clk::G12B_CLK_DOS_GCLK_HCODEC),
};
const std::vector<fdf::NodeProperty2> kClkDosHCodecProperties = std::vector{
    fdf::MakeProperty2(bind_fuchsia_hardware_clock::SERVICE,
                       bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty2(bind_fuchsia_clock::FUNCTION, bind_fuchsia_clock::FUNCTION_DOS_GCLK_HCODEC),
};

const std::vector<fdf::BindRule2> kClkDosRules = std::vector{
    fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_clock::SERVICE,
                             bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule2(bind_fuchsia::CLOCK_ID, g12b_clk::G12B_CLK_DOS),
};
const std::vector<fdf::NodeProperty2> kClkDosProperties = std::vector{
    fdf::MakeProperty2(bind_fuchsia_hardware_clock::SERVICE,
                       bind_fuchsia_hardware_clock::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty2(bind_fuchsia_clock::FUNCTION, bind_fuchsia_clock::FUNCTION_DOS),
};

static const fpbus::Node video_enc_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-video-enc";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_T931;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_VIDEO_ENC;
  dev.mmio() = sherlock_video_enc_mmios;
  dev.bti() = sherlock_video_enc_btis;
  dev.irq() = sherlock_video_enc_irqs;
  return dev;
}();

// TODO(b/42072838): Remove this duplicate pbus node once we finished migrating
// to composite node specs.
static const fpbus::Node video_enc_dev_old = []() {
  fpbus::Node dev = {};
  dev.name() = "aml-video-enc-old";
  dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  dev.pid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_PID_T931;
  dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_VIDEO_ENC;
  dev.instance_id() = 1;
  dev.mmio() = sherlock_video_enc_mmios;
  dev.bti() = sherlock_video_enc_btis;
  dev.irq() = sherlock_video_enc_irqs;
  return dev;
}();

zx_status_t Sherlock::VideoEncInit() {
  zxlogf(INFO, "video-enc init");

  fidl::Arena<> fidl_arena;

  std::vector<fdf::ParentSpec2> kVideoEncParents = {
      fdf::ParentSpec2{{kCanvasRules, kCanvasProperties}},
      fdf::ParentSpec2{{kClkDosHCodecRules, kClkDosHCodecProperties}},
      fdf::ParentSpec2{{kClkDosRules, kClkDosProperties}}};
  fdf::Arena arena('VIDE');
  auto composite_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, video_enc_dev),
      fidl::ToWire(fidl_arena, fuchsia_driver_framework::CompositeNodeSpec{
                                   {.name = "aml-video-enc", .parents2 = kVideoEncParents}}));
  if (!composite_result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec VideoEnc(video_enc_dev) request failed: %s",
           composite_result.FormatDescription().data());
    return composite_result.status();
  }
  if (composite_result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec VideoEnc(video_enc_dev) failed: %s",
           zx_status_get_string(composite_result->error_value()));
    return composite_result->error_value();
  }

  return ZX_OK;
}

}  // namespace sherlock

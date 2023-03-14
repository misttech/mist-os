// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/sysmem/cpp/bind.h>

#include "qemu-bus.h"
#include "qemu-virt.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace board_qemu_arm64 {
namespace fpbus = fuchsia_hardware_platform_bus;

const std::vector<fuchsia_driver_framework::BindRule> kSysmemRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_sysmem::BIND_PROTOCOL_DEVICE),
};

const std::vector<fuchsia_driver_framework::NodeProperty> kSysmemProperties = {
    fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_sysmem::BIND_PROTOCOL_DEVICE),
};

const std::vector<fuchsia_driver_framework::ParentSpec> kSysmemParents = {
    fuchsia_driver_framework::ParentSpec{
        {.bind_rules = kSysmemRules, .properties = kSysmemProperties}}};

zx_status_t QemuArm64::DisplayInit() {
  fpbus::Node display_dev;
  display_dev.name() = "display";
  display_dev.vid() = PDEV_VID_GENERIC;
  display_dev.pid() = PDEV_PID_GENERIC;
  display_dev.did() = PDEV_DID_FAKE_DISPLAY;
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('DISP');

  auto spec =
      fuchsia_driver_framework::CompositeNodeSpec{{.name = "sysmem", .parents = kSysmemParents}};
  fdf::WireUnownedResult display_result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, display_dev), fidl::ToWire(arena, spec));

  if (!display_result.ok()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Display(display_dev) request failed: %s", __func__,
           display_result.FormatDescription().data());
    return display_result.status();
  }
  if (display_result->is_error()) {
    zxlogf(ERROR, "%s: AddCompositeNodeSpec Display(display_dev) failed: %s", __func__,
           zx_status_get_string(display_result->error_value()));
    return display_result->error_value();
  }
  return ZX_OK;
}

}  // namespace board_qemu_arm64

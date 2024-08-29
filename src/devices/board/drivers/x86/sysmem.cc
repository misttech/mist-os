// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>

#include "x86.h"

namespace x86 {

namespace fpbus = fuchsia_hardware_platform_bus;

// This value is passed to bti_create as a marker; it does not have a particular
// meaning to anything in the system.
#define SYSMEM_BTI_ID 0x12341234UL

static const std::vector<fpbus::Bti> sysmem_btis = {
    {{
        .iommu_index = 0,
        .bti_id = SYSMEM_BTI_ID,
    }},
};

static const fpbus::Node GetSystemDev() {
  fpbus::Node node;
  node.name() = "sysmem";
  node.vid() = PDEV_VID_GENERIC;
  node.pid() = PDEV_PID_GENERIC;
  node.did() = PDEV_DID_SYSMEM;
  node.bti() = sysmem_btis;
  return node;
}

zx_status_t X86::SysmemInit() {
  auto sysmem_dev = GetSystemDev();

  fdf::Arena arena('X86S');
  fidl::Arena fidl_arena;
  auto status = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, sysmem_dev));
  if (!status.ok()) {
    zxlogf(ERROR, "%s: NodeAdd failed %s", __func__, status.FormatDescription().data());
    return status.status();
  }
  if (status->is_error()) {
    return status->error_value();
  }

  return ZX_OK;
}

}  // namespace x86

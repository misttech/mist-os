// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>

#include "nelson.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Bti> sysmem_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_SYSMEM,
    }},
};

static const std::vector<uint8_t> sysmem_metadata = [] {
  fuchsia_hardware_sysmem::Metadata metadata;
  metadata.vid() = PDEV_VID_AMLOGIC;
  metadata.pid() = PDEV_PID_AMLOGIC_S905D2;
  auto persist_result = fidl::Persist(metadata);
  // Given permitted values set above, we won't see failure here. An OOM would fail before getting
  // here.
  ZX_ASSERT(persist_result.is_ok());
  return std::move(persist_result.value());
}();

static const std::vector<fpbus::Metadata> sysmem_metadata_list{
    {{
        .type = fuchsia_hardware_sysmem::wire::kMetadataType,
        .data = sysmem_metadata,
    }},
};

static const fpbus::Node sysmem_dev = []() {
  fpbus::Node ret = {};
  ret.name() = "sysmem";
  ret.vid() = PDEV_VID_GENERIC;
  ret.pid() = PDEV_PID_GENERIC;
  ret.did() = PDEV_DID_SYSMEM;
  ret.bti() = sysmem_btis;
  ret.metadata() = sysmem_metadata_list;
  return ret;
}();

zx_status_t Nelson::SysmemInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SYSM');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, sysmem_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Sysmem(sysmem_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Sysmem(sysmem_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace nelson

// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>

#include "vim3.h"

namespace vim3 {
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
  metadata.pid() = PDEV_PID_AMLOGIC_A311D;
  metadata.protected_memory_size() = 0;

  // The AMlogic display engine needs contiguous physical memory for each
  // frame buffer, because it does not have a page table walker.
  //
  // The maximum supported resolution is documented below.
  // * "A311D Quick Reference Manual" revision 01, pages 2-3
  // * "A311D Datasheet" revision 08, section 2.2 "Features", pages 4-5
  //
  // These pages can be loaned back to zircon for use in pager-backed VMOs,
  // but these pages won't be used in "anonymous" VMOs (at least for now).
  // Whether the loaned-back pages can be absorbed by pager-backed VMOs is
  // workload dependent. The "k ppb stats_on" command can be used to determine
  // whether all loaned pages are being used by pager-backed VMOs.
  //
  // TODO(https://fxbug.dev/42072489): This should be configured elsewhere.
  metadata.contiguous_memory_size() = int64_t{200} * 1024 * 1024;

  auto persisted_result = fidl::Persist(metadata);
  // Given permitted values above, a failure won't be seen here. An OOM would
  // fail before getting here.
  ZX_ASSERT(persisted_result.is_ok());
  return std::move(persisted_result.value());
}();

static const std::vector<fpbus::Metadata> sysmem_metadata_list{
    {{
        .type = fuchsia_hardware_sysmem::kMetadataType,
        .data = sysmem_metadata,
    }},
};

static const fpbus::Node sysmem_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "sysmem";
  dev.vid() = PDEV_VID_GENERIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_SYSMEM;
  dev.bti() = sysmem_btis;
  dev.metadata() = sysmem_metadata_list;
  return dev;
}();

zx_status_t Vim3::SysmemInit() {
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
}  // namespace vim3

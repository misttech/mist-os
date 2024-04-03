// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/result.h>

#include "acpi-arm64.h"

namespace acpi_arm64 {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr size_t kBtiSysmem = 0;

zx::result<> AcpiArm64::SysmemInit() {
  static const std::vector<fpbus::Bti> kSysmemBtis{
      {{
          .iommu_index = 0,
          .bti_id = kBtiSysmem,
      }},
  };

  const fuchsia_hardware_sysmem::Metadata kSysmemMetadata = [] {
    fuchsia_hardware_sysmem::Metadata metadata;
    metadata.vid() = PDEV_VID_QEMU;
    metadata.pid() = PDEV_PID_QEMU;
    // no protected pool
    metadata.protected_memory_size() = 0;
    // -5 means 5% of physical RAM
    // we allocate a small amount of contiguous RAM to keep the sysmem tests from flaking,
    // see https://fxbug.dev/42146647.
    metadata.contiguous_memory_size() = -5;
    return metadata;
  }();

  auto metadata_result = fidl::Persist(kSysmemMetadata);
  ZX_ASSERT(metadata_result.is_ok());
  auto& metadata = metadata_result.value();

  static const std::vector<fpbus::Metadata> kSysmemMetadataList{
      {{
          .type = fuchsia_hardware_sysmem::kMetadataType,
          .data = std::move(metadata),
      }},
  };

  fpbus::Node sysmem_dev;
  sysmem_dev.name() = "sysmem";
  sysmem_dev.vid() = PDEV_VID_GENERIC;
  sysmem_dev.pid() = PDEV_PID_GENERIC;
  sysmem_dev.did() = PDEV_DID_SYSMEM;
  sysmem_dev.bti() = kSysmemBtis;
  sysmem_dev.metadata() = kSysmemMetadataList;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('ACPI');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, sysmem_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd AcpiArm64(sysmem_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return zx::error(result.status());
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd AcpiArm64(sysmem_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return zx::error(result->error_value());
  }

  return zx::ok();
}

}  // namespace acpi_arm64

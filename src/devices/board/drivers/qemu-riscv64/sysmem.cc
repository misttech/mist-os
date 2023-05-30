// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/sysmem/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/result.h>

#include "qemu-riscv64.h"

namespace board_qemu_riscv64 {
namespace fpbus = fuchsia_hardware_platform_bus;

zx::result<> QemuRiscv64::SysmemInit() {
  static const std::vector<fpbus::Bti> kSysmemBtis{
      {{
          .iommu_index = 0,
          .bti_id = BTI_SYSMEM,
      }},
  };

  constexpr sysmem_metadata_t kSysmemMetadata = {
      .vid = PDEV_VID_QEMU,
      .pid = PDEV_PID_QEMU,
      .protected_memory_size = 0,    // no protected pool
      .contiguous_memory_size = -5,  // 5% of physical ram

  };

  std::vector<fpbus::Metadata> kSysmemMetadataList{
      {{
          .type = SYSMEM_METADATA_TYPE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&kSysmemMetadata),
              reinterpret_cast<const uint8_t*>(&kSysmemMetadata) + sizeof(kSysmemMetadata)),
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
  fdf::Arena arena('SYSM');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, sysmem_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd request failed: %s", result.FormatDescription().data());
    return zx::make_result(result.status());
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd failed: %s", zx_status_get_string(result->error_value()));
    return result->take_error();
  }

  return zx::ok();
}

}  // namespace board_qemu_riscv64

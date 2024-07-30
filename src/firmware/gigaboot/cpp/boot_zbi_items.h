// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_CPP_BOOT_ZBI_ITEMS_H_
#define SRC_FIRMWARE_GIGABOOT_CPP_BOOT_ZBI_ITEMS_H_

#include <lib/abr/abr.h>
#include <lib/stdcompat/span.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi/zbi.h>
#include <lib/zircon_boot/zbi_utils.h>
#include <lib/zx/result.h>

#include <optional>
#include <variant>

namespace gigaboot {

// Context passed between zircon boot operation functions
struct ZbiContext {
  // Necessary for peripheral memory range
  std::optional<uint64_t> uart_mmio_phys = std::nullopt;
  // Necessary for peripheral memory range
  std::optional<std::variant<zbi_dcfg_arm_gic_v2_driver_t, zbi_dcfg_arm_gic_v3_driver_t>>
      gic_driver = std::nullopt;
  uint8_t num_cpu_nodes = 0;
};

// Add memory related zbi items. Note that once memory items are added, we must not do anything that
// can cause memory map changes, i.e. anything that involves memory allocation/de-allocation.
//
// Returns memory map key on success, which will be used for ExitBootService.
zx::result<size_t> AddMemoryItems(void *zbi, size_t capacity, const ZbiContext *context);
bool AddGigabootZbiItems(zbi_header_t *image, size_t capacity, const AbrSlotIndex *slot,
                         ZbiContext *context);
zbi_result_t AddBootloaderFiles(const char *name, const void *data, size_t len);
cpp20::span<uint8_t> GetZbiFiles();
void ClearBootloaderFiles();

}  // namespace gigaboot

#endif  // SRC_FIRMWARE_GIGABOOT_CPP_BOOT_ZBI_ITEMS_H_

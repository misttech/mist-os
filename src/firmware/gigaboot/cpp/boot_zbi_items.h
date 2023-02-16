// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_FIRMWARE_GIGABOOT_CPP_BOOT_ZBI_ITEMS_H_
#define SRC_FIRMWARE_GIGABOOT_CPP_BOOT_ZBI_ITEMS_H_

#include <lib/abr/abr.h>
#include <lib/stdcompat/span.h>
#include <lib/zbi/zbi.h>
#include <lib/zircon_boot/zbi_utils.h>

namespace gigaboot {
bool AddGigabootZbiItems(zbi_header_t *image, size_t capacity, const AbrSlotIndex *slot);
zbi_result_t AddBootloaderFiles(const char *name, const void *data, size_t len);
cpp20::span<uint8_t> GetZbiFiles();
void ClearBootloaderFiles();
}  // namespace gigaboot

#endif  // SRC_FIRMWARE_GIGABOOT_CPP_BOOT_ZBI_ITEMS_H_

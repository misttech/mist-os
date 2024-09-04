// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FALLOC_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FALLOC_H_

#include <ktl/optional.h>

#include <linux/falloc.h>

namespace starnix {

enum class FallocMode {
  Allocate,
  PunchHole,
  Collapse,
  Zero,
  InsertRange,
  UnshareRange,
};

class FallocModeHelper {
 public:
  static ktl::optional<FallocMode> from_bits(uint32_t mode) {
    // Translate mode values to corresponding FallocMode enum variants
    if (mode == 0) {
      return FallocMode::Allocate;
    } else if (mode == FALLOC_FL_KEEP_SIZE) {
      return FallocMode::Allocate;
    } else if (mode == (FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE)) {
      return FallocMode::PunchHole;
    } else if (mode == FALLOC_FL_COLLAPSE_RANGE) {
      return FallocMode::Collapse;
    } else if (mode == FALLOC_FL_ZERO_RANGE) {
      return FallocMode::Zero;
    } else if (mode == (FALLOC_FL_ZERO_RANGE | FALLOC_FL_KEEP_SIZE)) {
      return FallocMode::Zero;
    } else if (mode == FALLOC_FL_INSERT_RANGE) {
      return FallocMode::InsertRange;
    } else if (mode == FALLOC_FL_UNSHARE_RANGE) {
      return FallocMode::UnshareRange;
    } else {
      return ktl::nullopt;  // No matching FallocMode variant
    }
  }
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FALLOC_H_

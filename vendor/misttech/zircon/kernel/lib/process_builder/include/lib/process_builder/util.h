// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_UTIL_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_UTIL_H_

#include <zircon/types.h>

#include <arch/defines.h>

namespace process_builder::util {

/// Returns the starting address of the page that contains this address. For example, if page size
/// is 0x1000, page_start(0x3001) == page_start(0x3FAB) == 0x3000.
inline size_t page_start(size_t addr) { return addr & ~(static_cast<size_t>(PAGE_SIZE) - 1); }

/// Returns the offset of the address within its page. For example, if page size is 0x1000,
/// page_offset(0x2ABC) == page_offset(0x5ABC) == 0xABC.
inline size_t page_offset(size_t addr) { return addr & (static_cast<size_t>(PAGE_SIZE) - 1); }

/// Returns starting address of the next page after the one that contains this address, unless
/// address is already page aligned. For example, if page size is 0x1000, page_end(0x4001) ==
/// page_end(0x4FFF) == 0x5000, but page_end(0x4000) == 0x4000.
inline size_t page_end(size_t addr) {
  return page_start(addr + (static_cast<size_t>(PAGE_SIZE) - 1));
}

}  // namespace process_builder::util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_UTIL_H_

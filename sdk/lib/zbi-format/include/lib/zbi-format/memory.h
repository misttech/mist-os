// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_MEMORY_H_
#define LIB_ZBI_FORMAT_MEMORY_H_

#include <stdint.h>

#define ZBI_MEM_RANGE_RAM (1)
#define ZBI_MEM_RANGE_PERIPHERAL (2)
#define ZBI_MEM_RANGE_RESERVED (3)

// The ZBI_TYPE_MEM_CONFIG payload consist of one or more `zbi_mem_range_t`
// entries.
//
// The length of the item is `sizeof(zbi_mem_range_t)` times the number of
// entries. Each entry describes a contiguous range of memory:
//
//   * ZBI_MEM_RANGE_RAM ranges are standard RAM.
//
//   * ZBI_MEM_RANGE_PERIPHERAL are ranges that cover one or more devices.
//
//   * ZBI_MEM_RANGE_RESERVED are reserved ranges that should not be used by
//     the system. Reserved ranges may overlap previous or later
//     ZBI_MEM_RANGE_RAM or ZBI_MEM_RANGE_PERIPHERAL regions, in which case the
//     reserved range takes precedence.
//
//   * Any other `type` should be treated as `ZBI_MEM_RANGE_RESERVED` to allow
//     forwards compatibility.
//
// Entries in the table may be in any order, and only a single item of type
// ZBI_TYPE_MEM_CONFIG should be present in the ZBI.
typedef struct {
  uint64_t paddr;
  uint64_t length;
  uint32_t type;
  uint32_t reserved;
} zbi_mem_range_t;

// ZBI_TYPE_NVRAM payload.
typedef struct {
  uint64_t base;
  uint64_t length;
} zbi_nvram_t;

#endif  // LIB_ZBI_FORMAT_MEMORY_H_

// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "nandpart-utils.h"

#include <lib/ddk/debug.h>
#include <lib/stdcompat/bit.h>
#include <lib/stdcompat/span.h>
#include <lib/zbi-format/partition.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/assert.h>

#include <algorithm>

#include <fbl/algorithm.h>

// Checks that the partition map is valid, sorts it in partition order, and
// ensures blocks are on erase block boundaries.
zx_status_t SanitizePartitionMap(zbi_partition_map_t* pmap, const nand_info_t& nand_info) {
  if (pmap->partition_count == 0) {
    zxlogf(ERROR, "nandpart: partition count is zero");
    return ZX_ERR_INTERNAL;
  }

  static_assert(alignof(zbi_partition_map_t) >= alignof(zbi_partition_t));
  cpp20::span<zbi_partition_t> partitions(reinterpret_cast<zbi_partition_t*>(pmap + 1),
                                          pmap->partition_count);
  // 1) Last block must be greater than first for each partition entry.
  for (const zbi_partition_t& part : partitions) {
    if (part.first_block > part.last_block) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // 2) Partitions should be in (lexicographic) order.
  std::sort(partitions.begin(), partitions.end(), [](const auto& left, const auto& right) {
    return left.first_block < right.first_block ||
           (left.first_block == right.first_block && left.last_block < right.last_block);
  });

  // 3) Partitions should not be overlapping.
  for (size_t i = 1; i < partitions.size(); ++i) {
    const auto& left = partitions[i - 1];
    const auto& right = partitions[i];
    if (left.last_block >= right.first_block) {
      zxlogf(ERROR, "nandpart: partition %s [%lu, %lu] overlaps partition %s [%lu, %lu]", left.name,
             left.first_block, left.last_block, right.name, right.first_block, right.last_block);
      return ZX_ERR_INTERNAL;
    }
  }

  // 4) All partitions must start at an erase block boundary.
  const size_t erase_block_size = nand_info.page_size * nand_info.pages_per_block;
  ZX_DEBUG_ASSERT(cpp20::has_single_bit(erase_block_size));
  const int block_shift = ffs(static_cast<int>(erase_block_size)) - 1;

  if (pmap->block_size != erase_block_size) {
    for (auto& part : partitions) {
      uint64_t first_byte_offset = part.first_block * pmap->block_size;
      uint64_t last_byte_offset = (part.last_block + 1) * pmap->block_size;

      if (fbl::round_down(first_byte_offset, erase_block_size) != first_byte_offset ||
          fbl::round_down(last_byte_offset, erase_block_size) != last_byte_offset) {
        zxlogf(ERROR, "nandpart: partition %s size is not a multiple of erase_block_size",
               part.name);
        return ZX_ERR_INTERNAL;
      }
      part.first_block = first_byte_offset >> block_shift;
      part.last_block = (last_byte_offset >> block_shift) - 1;
    }
  }
  // 5) Partitions should exist within NAND.
  if (partitions.back().last_block >= nand_info.num_blocks) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  return ZX_OK;
}

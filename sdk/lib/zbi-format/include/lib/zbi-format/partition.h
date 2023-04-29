// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_PARTITION_H_
#define LIB_ZBI_FORMAT_PARTITION_H_

#include <stdint.h>

#define ZBI_PARTITION_NAME_LEN (32)
#define ZBI_PARTITION_GUID_LEN (16)

typedef struct {
  // GUID specifying the format and use of data stored in the partition.
  uint8_t type_guid[ZBI_PARTITION_GUID_LEN];

  // GUID unique to this partition.
  uint8_t uniq_guid[ZBI_PARTITION_GUID_LEN];

  // First and last block occupied by this partition.
  uint64_t first_block;
  uint64_t last_block;

  // Reserved for future use.  Set to 0.
  uint64_t flags;

  char name[ZBI_PARTITION_NAME_LEN];
} zbi_partition_t;

// ZBI_TYPE_DRV_PARTITION_MAP payload. This header is immediately followed by
// an array of the corresponding zbi_partition_t.
typedef struct {
  // Total blocks used on the device.
  uint64_t block_count;
  // Size of each block in bytes.
  uint64_t block_size;

  // Number of partitions in the map.
  uint32_t partition_count;

  // Reserved for future use.
  uint32_t reserved;

  // Device GUID.
  uint8_t guid[ZBI_PARTITION_GUID_LEN];
} zbi_partition_map_t;

#endif  // LIB_ZBI_FORMAT_PARTITION_H_

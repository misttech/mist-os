// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSCALLS_IOB_H_
#define ZIRCON_SYSCALLS_IOB_H_

#include <zircon/types.h>

__BEGIN_CDECLS

// A somewhat arbitrary limitation on the number of IOBuffer regions to protect us from having to
// handle IOBuffers with extremely large numbers of regions.
#define ZX_IOB_MAX_REGIONS 64

// TODO(fxb/118650): Clarify and finalize the ringbuffer disciplines.
#define ZX_IOB_DISCIPLINE_TYPE_NONE (0)

#define ZX_IOB_REGION_TYPE_PRIVATE (0)

typedef struct iob_discipline {
  uint64_t type;
  uint64_t reserved[8];
} zx_iob_discipline_t;

typedef struct iob_region_private {
  uint32_t options;
  uint32_t padding1;
  uint64_t padding2[3];
} zx_iob_region_private_t;

typedef struct iob_region {
  uint32_t type;
  uint32_t access;
  uint64_t size;
  zx_iob_discipline_t discipline;
  union {
    zx_iob_region_private_t private_region;
    uint8_t max_extension[32];
  };
} zx_iob_region_t;

// Access modifiers
#define ZX_IOB_EP0_CAN_MAP_READ (1 << 0)
#define ZX_IOB_EP0_CAN_MAP_WRITE (1 << 1)
#define ZX_IOB_EP0_CAN_MEDIATED_READ (1 << 2)
#define ZX_IOB_EP0_CAN_MEDIATED_WRITE (1 << 3)
#define ZX_IOB_EP1_CAN_MAP_READ (1 << 4)
#define ZX_IOB_EP1_CAN_MAP_WRITE (1 << 5)
#define ZX_IOB_EP1_CAN_MEDIATED_READ (1 << 6)
#define ZX_IOB_EP1_CAN_MEDIATED_WRITE (1 << 7)

__END_CDECLS

#endif  // ZIRCON_SYSCALLS_IOB_H_

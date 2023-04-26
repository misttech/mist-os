// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ZBI_FORMAT_BOARD_H_
#define LIB_ZBI_FORMAT_BOARD_H_

#include <stdint.h>

#define ZBI_BOARD_NAME_LEN 32

// ZBI_TYPE_PLATFORM_ID payload.
typedef struct {
  uint32_t vid;
  uint32_t pid;
  char board_name[ZBI_BOARD_NAME_LEN];
} zbi_platform_id_t;

// ZBI_TYPE_DRV_BOARD_INFO payload.
typedef struct {
  uint32_t revision;
} zbi_board_info_t;

#endif  // LIB_ZBI_FORMAT_BOARD_H_

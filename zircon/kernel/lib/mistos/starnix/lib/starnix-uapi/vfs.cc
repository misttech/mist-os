// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_uapi/vfs.h"

#include <arch/defines.h>

// clang-format off
#include <linux/limits.h>
// clang-format on

namespace starnix_uapi {

struct statfs default_statfs(uint32_t magic) {
  return {
      .f_type = static_cast<__kernel_long_t>(magic),
      .f_bsize = static_cast<__kernel_long_t>(PAGE_SIZE),
      .f_blocks = 0,
      .f_bfree = 0,
      .f_bavail = 0,
      .f_files = 0,
      .f_ffree = 0,
      .f_fsid = {0, 0},
      .f_namelen = static_cast<__kernel_long_t>(NAME_MAX),
      .f_frsize = static_cast<__kernel_long_t>(PAGE_SIZE),
      .f_flags = 0,
      .f_spare = {0, 0, 0, 0},
  };
}

}  // namespace starnix_uapi

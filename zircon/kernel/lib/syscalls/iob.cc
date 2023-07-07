// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/syscalls/forward.h>
#include <lib/user_copy/user_ptr.h>
#include <zircon/errors.h>
#include <zircon/types.h>

// zx_status_t zx_iob_create
zx_status_t sys_iob_create(uint64_t options, user_in_ptr<const void> regions, uint64_t num_regions,
                           user_out_handle* ep_out0, user_out_handle* ep_out1) {
  return ZX_ERR_NOT_SUPPORTED;
}

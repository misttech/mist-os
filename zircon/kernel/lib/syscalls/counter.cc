// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>

// zx_status_t zx_counter_create
zx_status_t sys_counter_create(uint32_t options, zx_handle_t* out) { return ZX_ERR_NOT_SUPPORTED; }

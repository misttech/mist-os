// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_ZIRCON_TESTS_RESTRICTED_MODE_SHARED_HELPERS_HELPERS_H_
#define SRC_ZIRCON_TESTS_RESTRICTED_MODE_SHARED_HELPERS_HELPERS_H_

#include <zircon/syscalls-next.h>

// Because zx_restricted_enter doesn't return like normal syscalls,
// we use a wrapper function to make it easier to use.
extern "C" zx_status_t restricted_enter_wrapper(uint32_t options,
                                                zx_restricted_reason_t* reason_code);

#endif  // SRC_ZIRCON_TESTS_RESTRICTED_MODE_SHARED_HELPERS_HELPERS_H_

// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STATUS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STATUS_H_

#include <zircon/types.h>

#ifdef __cplusplus
extern "C" {
#endif

// Given one of the status codes defined in <zircon/errors.h> (ZX_ERR_* or
// ZX_OK), this function returns an identifier string for the status code.
//
// For example, zx_status_get_string(ZX_ERR_TIMED_OUT) returns the string
// "ZX_ERR_TIMED_OUT".
const char* zx_status_get_string(zx_status_t status);

#ifdef __cplusplus
}
#endif

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STATUS_H_

// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ERROR_PROPAGATION_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ERROR_PROPAGATION_H_

#include <lib/fit/result.h>
#include <trace.h>

// Question mark (error propagation)
#define _EP(result)               \
  ;                               \
  do {                            \
    if (result.is_error()) {      \
      return result.take_error(); \
    }                             \
  } while (0)

#define _EP_MSG(result, x...)     \
  ;                               \
  do {                            \
    if (result.is_error()) {      \
      TRACEF(x);                  \
      return result.take_error(); \
    }                             \
  } while (0)

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ERROR_PROPAGATION_H_

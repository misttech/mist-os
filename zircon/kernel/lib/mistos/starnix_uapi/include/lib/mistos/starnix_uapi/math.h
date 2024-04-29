// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_MATH_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_MATH_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>

#include <ktl/optional.h>

namespace starnix_uapi {

fit::result<Errno, size_t> round_up_to_increment(size_t size, size_t increment);
fit::result<Errno, size_t> round_up_to_system_page_size(size_t size);

template <typename T>
inline ktl::optional<T> checked_add(const T& lhs, const T& rhs) {
  T result;
  if (add_overflow(lhs, rhs, &result)) {
    return ktl::nullopt;
  }
  return result;
}

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_MATH_H_

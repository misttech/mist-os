// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_VALUE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_VALUE_H_

#include <lib/mistos/util/try_from.h>
#include <zircon/types.h>

#include <ktl/optional.h>

namespace starnix_uapi {

/// A value Starnix has received from userspace.
///
/// Typically, these values are received in syscall arguments and need to be validated before they
/// can be used directly. For example, integers need to be checked for overflow during arithmetical
/// operation.
template <typename T>
class UserValue {
 public:
  /// Create a UserValue from a raw value provided by userspace.
  static UserValue from_raw(T raw) { return {.value_ = raw}; }

  /// The raw value that the user provided.
  T raw() { return value_; }

  /// Attempt to convert this value into another type.
  template <typename U>
  ktl::optional<U> try_into() const {
    return mtl::TryFrom<T, U>(value_);
  }

  explicit UserValue(T value) : value_(value) {}

 private:
  T value_;
};

}  // namespace starnix_uapi

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_USER_VALUE_H_

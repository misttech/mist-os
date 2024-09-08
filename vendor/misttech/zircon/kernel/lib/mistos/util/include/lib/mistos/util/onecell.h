// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ONECELL_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ONECELL_H_

#include <kernel/mutex.h>
#include <ktl/optional.h>

template <typename T>
class OnceCell {
 public:
  OnceCell() = default;

  bool is_initialized() const {
    Guard<Mutex> lock(&mtx_);
    return value_.has_value();
  }

  template <typename... Args>
  bool set(Args&&... args) {
    Guard<Mutex> lock(&mtx_);
    if (value_.has_value())
      return false;
    value_.emplace(std::forward<Args>(args)...);
    return true;
  }

  T& get() {
    Guard<Mutex> lock(&mtx_);
    DEBUG_ASSERT_MSG(value_.has_value(), "Value not initialized");
    return *value_;
  }

  const T& get() const {
    Guard<Mutex> lock(&mtx_);
    DEBUG_ASSERT_MSG(value_.has_value(), "Value not initialized");
    return *value_;
  }

  ktl::optional<T> take() {
    Guard<Mutex> lock(&mtx_);
    return std::move(value_);
  }

 private:
  mutable DECLARE_MUTEX(OnceCell) mtx_;

  ktl::optional<T> value_ __TA_GUARDED(mtx_);
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ONECELL_H_

// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_ARC_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_ARC_H_

#include <lib/mistos/zx/object.h>

#include <fbl/ref_counted.h>
#include <ktl/move.h>

namespace zx {

/// A thread-safe reference-counting pointer. 'Arc' stands for 'Atomically
/// Reference Counted'.

template <typename T>
class Arc : public fbl::RefCounted<Arc<T>> {
 public:
  Arc(T value) : value_(ktl::move(value)) {}
  ~Arc() = default;
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(Arc);

  const T& operator*() const { return value_; }
  const T* operator->() const { return &value_; }
  const T& as_ref() const { return value_; }
  T& as_ref_mut() { return value_; }

 private:
  T value_;
};

template <typename T>
bool operator==(const Arc<T>& a, const Arc<T>& b) {
  return a->get() == b->get();
}

template <typename T>
bool operator!=(const Arc<T>& a, const Arc<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(const Arc<T>& a, const Arc<T>& b) {
  return a->get() < b->get();
}

template <typename T>
bool operator>(const Arc<T>& a, const Arc<T>& b) {
  return a->get() > b->get();
}

template <typename T>
bool operator<=(const Arc<T>& a, const Arc<T>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const Arc<T>& a, const Arc<T>& b) {
  return !(a < b);
}

template <typename T>
bool operator==(zx_handle_t a, const Arc<T>& b) {
  return a == b->get();
}

template <typename T>
bool operator!=(zx_handle_t a, const Arc<T>& b) {
  return !(a == b);
}

template <typename T>
bool operator<(zx_handle_t a, const Arc<T>& b) {
  return a < b->get();
}

template <typename T>
bool operator>(zx_handle_t a, const Arc<T>& b) {
  return a > b->get();
}

template <typename T>
bool operator<=(zx_handle_t a, const Arc<T>& b) {
  return !(a > b);
}

template <typename T>
bool operator>=(zx_handle_t a, const Arc<T>& b) {
  return !(a < b);
}

template <typename T>
bool operator==(const Arc<T>& a, zx_handle_t b) {
  return a->get() == b;
}

template <typename T>
bool operator!=(const Arc<T>& a, zx_handle_t b) {
  return !(a == b);
}

template <typename T>
bool operator<(const Arc<T>& a, zx_handle_t b) {
  return a->get() < b;
}

template <typename T>
bool operator>(const Arc<T>& a, zx_handle_t b) {
  return a->get() > b;
}

template <typename T>
bool operator<=(const Arc<T>& a, zx_handle_t b) {
  return !(a > b);
}

template <typename T>
bool operator>=(const Arc<T>& a, zx_handle_t b) {
  return !(a < b);
}

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_ARC_H_

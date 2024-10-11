// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_DEREF_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_DEREF_H_

namespace mtl {

template <typename T>
class Deref {
 public:
  Deref(T* ptr) : ptr_(ptr) {}

  // Overloading the dereference operator
  T& operator*() const { return *ptr_; }

  // Overloading the pointer-to-member operator
  T* operator->() const { return ptr_; }

 private:
  T* ptr_;
};

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_DEREF_H_

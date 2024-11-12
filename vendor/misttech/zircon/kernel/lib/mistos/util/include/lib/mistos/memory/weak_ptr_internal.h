// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_MEMORY_WEAK_PTR_INTERNAL_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_MEMORY_WEAK_PTR_INTERNAL_H_

#include <fbl/auto_lock.h>
#include <fbl/ref_counted.h>
#include <kernel/lockdep.h>

namespace mtl::internal {

// |WeakPtr<T>|s have a reference to a |WeakPtrFlag| to determine whether they
// are valid (non-null) or not. We do not store a |T*| in this object since
// there may also be |WeakPtr<U>|s to the same object, where |U| is a superclass
// of |T|.
//
// This class in not thread-safe, though references may be released on any
// thread (allowing weak pointers to be destroyed/reset/reassigned on any
// thread).
class WeakPtrFlag : public fbl::RefCounted<WeakPtrFlag> {
 public:
  WeakPtrFlag();
  ~WeakPtrFlag();

  bool is_valid() const {
    Guard<fbl::Mutex> guard{lock()};
    return is_valid_;
  }

  bool is_valid_locked() const { return is_valid_; }

  void Invalidate();

  Lock<fbl::Mutex>* lock() const TA_RET_CAP(lock_) { return &lock_; }
  Lock<fbl::Mutex>& lock_ref() const TA_RET_CAP(lock_) { return lock_; }

 private:
  mutable LOCK_DEP_INSTRUMENT(WeakPtrFlag, fbl::Mutex) lock_;

  bool is_valid_;

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(WeakPtrFlag);
};

}  // namespace mtl::internal

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_MEMORY_WEAK_PTR_INTERNAL_H_

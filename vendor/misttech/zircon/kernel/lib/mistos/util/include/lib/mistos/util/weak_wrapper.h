// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_WEAK_WRAPPER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_WEAK_WRAPPER_H_

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted_upgradeable.h>

namespace util {

template <typename T>
class WeakPtr final {
 public:
  using element_type = T;

  // Constructors
  constexpr WeakPtr() : ptr_(nullptr) {}
  constexpr WeakPtr(decltype(nullptr)) : WeakPtr() {}
  explicit WeakPtr(T* p) : ptr_(p) {}

  // Copy construction.
  WeakPtr(const WeakPtr& r) : WeakPtr(r.ptr_) {}

  // Assignment
  WeakPtr& operator=(const WeakPtr& r) {
    fbl::AutoLock lock1(&lock_);
    fbl::AutoLock lock2(&r.lock_);
    ptr_ = r.ptr_;
    return *this;
  }

  // Move construction
  WeakPtr(WeakPtr&& r) : ptr_(r.ptr_) {
    fbl::AutoLock lock(&r.lock_);
    r.ptr_ = nullptr;
  }

  // Move assignment
  WeakPtr& operator=(WeakPtr&& r) {
    WeakPtr(std::move(r)).swap(*this);
    return *this;
  }

  fbl::RefPtr<T> Lock() const {
    fbl::AutoLock lock(&lock_);
    if (ptr_)
      return fbl::MakeRefPtrUpgradeFromRaw(ptr_, lock_);
    return fbl::RefPtr<T>();
  }

  ~WeakPtr() {
    fbl::AutoLock lock(&lock_);
    ptr_ = nullptr;
  }

  void reset(T* ptr = nullptr) { WeakPtr(ptr).swap(*this); }

  T* release() __WARN_UNUSED_RESULT {
    fbl::AutoLock lock(&lock_);
    T* result = ptr_;
    ptr_ = nullptr;
    return result;
  }

  void swap(WeakPtr& r) {
    fbl::AutoLock lock1(&lock_);
    fbl::AutoLock lock2(&r.lock_);
    T* p = ptr_;
    ptr_ = r.ptr_;
    r.ptr_ = p;
  }

  T* get() const {
    fbl::AutoLock lock(&lock_);
    return ptr_;
  }

  T& operator*() const {
    fbl::AutoLock lock(&lock_);
    return *ptr_;
  }

  T* operator->() const {
    fbl::AutoLock lock(&lock_);
    return ptr_;
  }

  explicit operator bool() const {
    fbl::AutoLock lock(&lock_);
    return !!ptr_;
  }

  // Comparison against nullptr operators (of the form, myptr == nullptr).
  bool operator==(decltype(nullptr)) const {
    fbl::AutoLock lock(&lock_);
    return (ptr_ == nullptr);
  }

  bool operator!=(decltype(nullptr)) const {
    fbl::AutoLock lock(&lock_);
    return (ptr_ != nullptr);
  }

  bool operator==(const WeakPtr<T>& other) const {
    fbl::AutoLock lock(&lock_);
    fbl::AutoLock other_lock(&other.lock_);
    return ptr_ == other.ptr_;
  }

  bool operator!=(const WeakPtr<T>& other) const {
    fbl::AutoLock lock(&lock_);
    fbl::AutoLock other_lock(&other.lock_);
    return ptr_ != other.ptr_;
  }

 private:
  mutable fbl::Mutex lock_;
  T* ptr_ __TA_GUARDED(lock_) = nullptr;
};

// Comparison against nullptr operator (of the form, nullptr == myptr)
template <typename T>
static inline bool operator==(decltype(nullptr), const WeakPtr<T>& ptr) {
  return (ptr.get() == nullptr);
}

template <typename T>
static inline bool operator!=(decltype(nullptr), const WeakPtr<T>& ptr) {
  return (ptr.get() != nullptr);
}

}  // namespace util

#include <fbl/intrusive_pointer_traits.h>

namespace fbl::internal {

// Traits for managing util::weak_ptr pointers.
template <typename T>
struct ContainerPtrTraits<::util::WeakPtr<T>> {
  using ValueType = T;
  using RefType = T&;
  using ConstRefType = const T&;
  using PtrType = ::util::WeakPtr<T>;
  using ConstPtrType = ::util::WeakPtr<const T>;
  using RawPtrType = T*;
  using ConstRawPtrType = const T*;

  static constexpr bool IsManaged = true;
  static constexpr bool CanCopy = true;

  static inline T* GetRaw(const PtrType& ptr) { return ptr.get(); }
  static inline PtrType Copy(const RawPtrType& ptr) { return PtrType(ptr); }

  static inline RawPtrType Leak(PtrType& ptr) __WARN_UNUSED_RESULT { return ptr.release(); }

  static inline PtrType Reclaim(RawPtrType ptr) { return PtrType(ptr); }
};

}  // namespace fbl::internal

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_WEAK_WRAPPER_H_

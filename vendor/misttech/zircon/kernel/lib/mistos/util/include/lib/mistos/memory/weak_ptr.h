// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// This file provides weak pointers and weak pointer factories that work like
// Chromium's |base::WeakPtr<T>| and |base::WeakPtrFactory<T>|. Note that we do
// not provide anything analogous to |base::SupportsWeakPtr<T>|.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MEMORY_WEAK_PTR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MEMORY_WEAK_PTR_H_

#include <inttypes.h>
#include <zircon/assert.h>

#include <cstddef>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>

#include "lib/mistos/memory/weak_ptr_internal.h"

namespace mtl {

// Forward declaration, so |WeakPtr<T>| can friend it.
template <typename T>
class WeakPtrFactory;

// Class for "weak pointers" that can be invalidated. Valid weak pointers can
// only originate from a |WeakPtrFactory| (see below), though weak pointers are
// copyable and movable.
//
// Weak pointers are not in general thread-safe. They may only be *used* on a
// single thread, namely the same thread as the "originating" |WeakPtrFactory|
// (which can invalidate the weak pointers that it generates).
//
// However, weak pointers may be passed to other threads, reset on other
// threads, or destroyed on other threads. They may also be reassigned on other
// threads (in which case they should then only be used on the thread
// corresponding to the new "originating" |WeakPtrFactory|).
template <typename T>
class WeakPtr {
 public:
  using element_type = T;

  WeakPtr() : ptr_(nullptr) {}
  WeakPtr(std::nullptr_t) : WeakPtr() {}

  // Copy constructor.
  WeakPtr(const WeakPtr<T>& r) = default;

  template <typename U>
  WeakPtr(const WeakPtr<U>& r) : ptr_(r.ptr_), flag_(r.flag_) {}

  // Move constructor.
  WeakPtr(WeakPtr<T>&& r) = default;

  template <typename U>
  WeakPtr(WeakPtr<U>&& r) : ptr_(std::exchange(r.ptr_, nullptr)), flag_(std::move(r.flag_)) {}

  ~WeakPtr() = default;

  fbl::RefPtr<T> Lock() const {
    if (flag_) {
      Guard<fbl::Mutex> guard{flag_->lock()};
      if (flag_->is_valid_locked()) {
        return fbl::MakeRefPtrUpgradeFromRaw(ptr_, flag_->lock_ref());
      }
    }
    return fbl::RefPtr<T>();
  }

  // The following methods are thread-friendly, in the sense that they may be
  // called subject to additional synchronization.

  // Copy assignment.
  WeakPtr<T>& operator=(const WeakPtr<T>& r) = default;

  // Move assignment.
  WeakPtr<T>& operator=(WeakPtr<T>&& r) = default;

  void reset() {
    if (flag_) {
      Guard<fbl::Mutex> guard{flag_->lock()};
      flag_ = nullptr;
    }
  }

  explicit operator bool() const { return flag_ && flag_->is_valid(); }

  T* get() const { return *this ? ptr_ : nullptr; }

  T& operator*() const {
    ZX_DEBUG_ASSERT(*this);
    return *get();
  }

  T* operator->() const {
    ZX_DEBUG_ASSERT(*this);
    return get();
  }

  bool operator==(decltype(nullptr)) const { return (ptr_ == nullptr); }

  bool operator!=(decltype(nullptr)) const { return (ptr_ != nullptr); }

  bool operator==(const WeakPtr<T>& other) const { return ptr_ == other.ptr_; }

  bool operator!=(const WeakPtr<T>& other) const { return ptr_ != other.ptr_; }

 private:
  template <typename U>
  friend class WeakPtr;

  friend class WeakPtrFactory<T>;

  explicit WeakPtr(T* ptr, fbl::RefPtr<internal::WeakPtrFlag>&& flag)
      : ptr_(ptr), flag_(std::move(flag)) {}

  T* ptr_;

  fbl::RefPtr<internal::WeakPtrFlag> flag_;

  // Copy/move construction/assignment supported.
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

// Class that produces (valid) |WeakPtr<T>|s. Typically, this is used as a
// member variable of |T| (preferably the last one -- see below), and |T|'s
// methods control how weak pointers to it are vended
//
// Example:
//
//  class Controller {
//   public:
//    Controller() : ..., weak_factory_(this) {}
//    ...
//
//    void SpawnWorker() { Worker::StartNew(weak_factory_.GetWeakPtr()); }
//    void WorkComplete(const Result& result) { ... }
//
//   private:
//    ...
//
//    // Member variables should appear before the |WeakPtrFactory|, to ensure
//    // that any |WeakPtr|s to |Controller| are invalidated before its member
//    // variables' destructors are executed.
//    WeakPtrFactory<Controller> weak_factory_;
//  };
//
//  class Worker {
//   public:
//    static void StartNew(const WeakPtr<Controller>& controller) {
//      Worker* worker = new Worker(controller);
//      // Kick off asynchronous processing....
//    }
//
//   private:
//    Worker(const WeakPtr<Controller>& controller) : controller_(controller) {}
//
//    void DidCompleteAsynchronousProcessing(const Result& result) {
//      if (controller_)
//        controller_->WorkComplete(result);
//    }
//
//    WeakPtr<Controller> controller_;
//  };
template <typename T>
class WeakPtrFactory {
 public:
  explicit WeakPtrFactory(T* ptr) : ptr_(ptr) { ZX_DEBUG_ASSERT(ptr_); }
  ~WeakPtrFactory() {
    InvalidateWeakPtrs();
    ZX_DEBUG_ASSERT(Poison());
  }

  // Gets a new weak pointer, which will be valid until either
  // |InvalidateWeakPtrs()| is called or this object is destroyed.
  WeakPtr<T> GetWeakPtr() {
    ZX_DEBUG_ASSERT(reinterpret_cast<uintptr_t>(ptr_) != kPoisonedPointer);
    fbl::AutoLock lock(&lock_);
    if (!flag_) {
      fbl::AllocChecker ac;
      flag_ = fbl::MakeRefCountedChecked<internal::WeakPtrFlag>(&ac);
      ZX_ASSERT(ac.check());
    }
    return WeakPtr<T>(ptr_, fbl::RefPtr<internal::WeakPtrFlag>(flag_));
  }

  // Call this method to invalidate all existing weak pointers. (Note that
  // additional weak pointers can be produced even after this is called.)
  void InvalidateWeakPtrs() {
    fbl::AutoLock lock(&lock_);
    if (!flag_)
      return;
    flag_->Invalidate();
    flag_ = nullptr;
  }

  // Call this method to determine if any weak pointers exist. (Note that a
  // "false" result is definitive, but a "true" result may not be if weak
  // pointers are held/reset/destroyed/reassigned on other threads.)
  bool HasWeakPtrs() const {
    fbl::AutoLock lock(&lock_);
    return flag_ && !flag_->IsLastReference();
  }

 private:
  bool Poison() {
    *reinterpret_cast<uintptr_t volatile*>(const_cast<T**>(&ptr_)) = kPoisonedPointer;
    return kPoisonedPointer;
  }

  // Value to poison |ptr_| with in debug mode to ensure that weak pointer are
  // not generated once this class has been destroyed.
  // Value must be different from 0, and invalid as a real pointer address.
  static constexpr uintptr_t kPoisonedPointer = 1;
  // Note: See weak_ptr_internal.h for an explanation of why we store the
  // pointer here, instead of in the "flag".
  T* const ptr_;

  mutable fbl::Mutex lock_;
  fbl::RefPtr<internal::WeakPtrFlag> flag_ __TA_GUARDED(lock_);

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(WeakPtrFactory);
};

}  // namespace mtl
#include <fbl/intrusive_pointer_traits.h>

namespace fbl::internal {

// Traits for managing util::weak_ptr pointers.
template <typename T>
struct ContainerPtrTraits<::mtl::WeakPtr<T>> {
  using ValueType = T;
  using RefType = T&;
  using ConstRefType = const T&;
  using PtrType = ::mtl::WeakPtr<T>;
  using ConstPtrType = ::mtl::WeakPtr<const T>;
  using RawPtrType = T*;
  using ConstRawPtrType = const T*;

  static constexpr bool IsManaged = true;
  static constexpr bool CanCopy = true;

  static inline T* GetRaw(const PtrType& ptr) { return ptr.get(); }

  static inline PtrType Copy(const RawPtrType& ptr) {
    auto copy = ::fbl::RefPtr<T>(ptr);
    if (copy) {
      return copy->weak_factory_.GetWeakPtr();
    }
    return ::mtl::WeakPtr<T>();
  }

  static inline RawPtrType Leak(PtrType& ptr) __WARN_UNUSED_RESULT {
    auto strong = ptr.Lock();
    if (strong) {
      T* result = ::fbl::ExportToRawPtr(&strong);
      ptr.reset();
      return result;
    }
    return nullptr;
  }

  static inline PtrType Reclaim(RawPtrType ptr) {
    auto ref_counted = ::fbl::ImportFromRawPtr(ptr);
    if (ref_counted) {
      return ptr->weak_factory_.GetWeakPtr();
    }
    return ::mtl::WeakPtr<T>();
  }
};

}  // namespace fbl::internal

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MEMORY_WEAK_PTR_H_

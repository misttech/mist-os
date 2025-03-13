// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_LOCKS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_LOCKS_H_

#include <lib/mistos/memory/weak_ptr.h>
#include <zircon/compiler.h>

#include <fbl/ref_counted_upgradeable.h>
#include <kernel/brwlock.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>

namespace starnix_sync {

template <typename Data>
class MutexGuard;
template <typename Data>
class MappedMutexGuard;

template <typename Data>
class Mutex : public fbl::RefCountedUpgradeable<Mutex<Data>> {
 public:
  Mutex() : weak_factory_(this) {}
  explicit Mutex(Data&& data) : data_(data), weak_factory_(this) {}

  MutexGuard<Data> Lock() { return MutexGuard(this); }

 private:
  // No moving or copying allowed.
  DISALLOW_COPY_ASSIGN_AND_MOVE(Mutex);

  friend class MutexGuard<Data>;
  friend class MappedMutexGuard<Data>;

  DECLARE_MUTEX(Mutex) lock_;
  Data data_ __TA_GUARDED(lock_);

 public:
  mtl::WeakPtrFactory<Mutex<Data>> weak_factory_;  // must be last
};

/// An RAII mutex guard returned by `MutexGuard::map`, which can point to a
/// subfield of the protected data.
template <typename Data>
class MappedMutexGuard {
 public:
  MappedMutexGuard() = default;
  ~MappedMutexGuard() {}
  MappedMutexGuard(Data* ptr) : ptr_(ptr) {}
  MappedMutexGuard(MappedMutexGuard&& other) : ptr_(other.ptr_) { other.ptr_ = nullptr; }

  /*
    MappedMutexGuard& operator=(MappedMutexGuard&& other) {
      guard_ = ktl::move(other.guard_);
      mtx_ = other.mtx_;
      other.mtx_ = nullptr;
      return *this;
    }
  */

  Data* operator->() const {
    ZX_ASSERT(ptr_);
    return ptr_;
  }
  Data* operator->() {
    ZX_ASSERT(ptr_);
    return ptr_;
  }

  Data& operator*() const {
    ZX_ASSERT(ptr_);
    return *ptr_;
  }
  Data& operator*() {
    ZX_ASSERT(ptr_);
    return *ptr_;
  }

 private:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(MappedMutexGuard);
  Data* ptr_ = nullptr;
};

template <typename Data>
class MutexGuard {
 public:
  MutexGuard() = default;
  ~MutexGuard() { release(); }
  MutexGuard(MutexGuard&& other) : mtx_(other.mtx_), lock_(other.take_lock()) {}
  MutexGuard(Mutex<Data>* mtx) : mtx_(mtx), lock_(Guard<::Mutex>(&mtx->lock_).take()) {}

  // Take both the pointer and the lock, leaving the MutexGuard empty. Caller must take ownership
  // of the returned lock and release it.
  ktl::pair<Mutex<Data>*, Guard<::Mutex>::Adoptable> take() {
    Mutex<Data>* ret = mtx_;
    return {ret, take_lock()};
  }

  // Release the lock, returning the underlying pointer.
  Mutex<Data>* release() {
    auto [ret, lock] = take();
    if (ret) {
      Guard<::Mutex> guard{AdoptLock, &ret->lock_, ktl::move(lock)};
    }
    return ret;
  }

  MutexGuard& operator=(MutexGuard&& other) {
    if (this != &other) {
      mtx_ = other.mtx_;
      lock_ = ktl::move(other.take_lock());
    }
    return *this;
  }

  template <typename U, typename F>
  static MappedMutexGuard<U> map(MutexGuard&& self, F&& f) __TA_NO_THREAD_SAFETY_ANALYSIS {
    auto* data = f(&self.mtx_->data_);
    return MappedMutexGuard<U>(data);
  }

  Data* operator->() const {
    ZX_ASSERT(mtx_);
    return &mtx_->data_;
  }
  Data* operator->() {
    ZX_ASSERT(mtx_);
    return &mtx_->data_;
  }

  Data& operator*() const {
    ZX_ASSERT(mtx_);
    return mtx_->data_;
  }
  Data& operator*() {
    ZX_ASSERT(mtx_);
    return mtx_->data_;
  }

 private:
  // Helper for moving out the lock_ and clearing the data_ at the same time.
  Guard<::Mutex>::Adoptable&& take_lock() {
    mtx_ = nullptr;
    return ktl::move(lock_);
  }

  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(MutexGuard);
  Mutex<Data>* mtx_ = nullptr;
  Guard<::Mutex>::Adoptable lock_;
};

template <typename Data, typename Option>
class RwLockGuard;

template <typename Data>
class RwLock {
 public:
  using RwLockWriteGuard = RwLockGuard<Data, BrwLockPi::Writer>;
  using RwLockReadGuard = RwLockGuard<Data, BrwLockPi::Reader>;

  RwLock() = default;
  explicit RwLock(Data&& data) : data_(data) {}

  RwLockReadGuard Read() const { return RwLockReadGuard(this); }
  RwLockWriteGuard Write() { return RwLockWriteGuard(this); }

 private:
  // No moving or copying allowed.
  DISALLOW_COPY_ASSIGN_AND_MOVE(RwLock);

  friend class RwLockGuard<Data, BrwLockPi::Reader>;
  friend class RwLockGuard<Data, BrwLockPi::Writer>;

  mutable DECLARE_BRWLOCK_PI(RwLock) lock_;
  Data data_ __TA_GUARDED(lock_);
};

template <typename Data, typename Option>
class RwLockGuard {
 public:
  RwLockGuard(std::conditional_t<std::is_same_v<Option, BrwLockPi::Reader>, const RwLock<Data>*,
                                 RwLock<Data>*>
                  mtx)
      : guard_(&mtx->lock_), mtx_(mtx) {}

  template <typename T = Option>
  RwLockGuard(RwLockGuard&& other) noexcept
    requires std::is_same_v<T, BrwLockPi::Writer>
      : guard_(AdoptLock, &other.mtx_->lock_, other.guard_.take()), mtx_(other.mtx_) {
    other.mtx_ = nullptr;
  }

  template <typename T = Option>
  RwLockGuard(RwLockGuard&& other) noexcept
    requires std::is_same_v<T, BrwLockPi::Reader>
      : guard_(&other.mtx_->lock_), mtx_(other.mtx_) {
    other.mtx_ = nullptr;
  }

  std::conditional_t<std::is_same_v<Option, BrwLockPi::Reader>, const Data*, Data*> operator->()
      const {
    return &mtx_->data_;
  }
  std::conditional_t<std::is_same_v<Option, BrwLockPi::Reader>, const Data&, Data&> operator*()
      const {
    return mtx_->data_;
  }

  ~RwLockGuard() = default;

 private:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(RwLockGuard);
  Guard<BrwLockPi, Option> guard_;
  std::conditional_t<std::is_same_v<Option, BrwLockPi::Reader>, const RwLock<Data>*, RwLock<Data>*>
      mtx_;
};

}  // namespace starnix_sync

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_LOCKS_H_

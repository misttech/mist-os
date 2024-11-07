// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_LOCKS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_LOCKS_H_

#include <zircon/compiler.h>

#include <fbl/ref_counted_upgradeable.h>
#include <kernel/brwlock.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>

namespace starnix_sync {

template <typename Data>
class MutexGuard;

template <typename Data>
class StarnixMutex : public fbl::RefCountedUpgradeable<StarnixMutex<Data>> {
 public:
  StarnixMutex() = default;
  explicit StarnixMutex(Data&& data) : data_(data) {}

  MutexGuard<Data> Lock() { return MutexGuard(this); }
  MutexGuard<Data> Lock() const { return MutexGuard(this); }

 private:
  // No moving or copying allowed.
  DISALLOW_COPY_ASSIGN_AND_MOVE(StarnixMutex);

  friend class MutexGuard<Data>;

  mutable DECLARE_MUTEX(StarnixMutex) lock_;
  Data data_ __TA_GUARDED(lock_);
};

// template <typename Data>
// using StarnixMutex = Mutex<Data>;

/// An RAII mutex guard returned by `MutexGuard::map`, which can point to a
/// subfield of the protected data.
template <typename Data>
class MappedMutexGuard : public Guard<::Mutex> {
 public:
  __WARN_UNUSED_CONSTRUCTOR explicit MappedMutexGuard(Guard<::Mutex>&& adopt, Data* data)
      __TA_ACQUIRE(adopt.lock())
      : Guard(AdoptLock, ktl::move(adopt)), data_(data) {}

  MappedMutexGuard(MappedMutexGuard&& other)
      : Guard(AdoptLock, ktl::move(other.take())), data_(other.data_) {
    other.data_ = nullptr;
  }

  MappedMutexGuard& operator=(MappedMutexGuard&& other) {
    if (this != &other) {
      Guard::operator=(ktl::move(other.take()));
      data_ = other.data_;
      other.data_ = nullptr;
    }
    return *this;
  }

  Data* operator->() const { return data_; }
  Data* operator->() { return data_; }

  Data& operator*() const {
    DEBUG_ASSERT(data_ != nullptr);
    return *data_;
  }
  Data& operator*() {
    DEBUG_ASSERT(data_ != nullptr);
    return *data_;
  }

  ~MappedMutexGuard() = default;

 private:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(MappedMutexGuard);
  Data* data_;
};

template <typename Data>
class MutexGuard : public Guard<::Mutex> {
 public:
  __WARN_UNUSED_CONSTRUCTOR explicit MutexGuard(StarnixMutex<Data>* mtx)
      : Guard{&mtx->lock_}, data_(&mtx->data_) {}

  MutexGuard(MutexGuard&& other) : Guard(AdoptLock, ktl::move(other.take())), data_(other.data_) {
    other.data_ = nullptr;
  }

  MutexGuard& operator=(MutexGuard&& other) {
    if (this != &other) {
      Guard::operator=(ktl::move(other.take()));
      data_ = other.data_;
      other.data_ = nullptr;
      return *this;
    }
  }

  template <typename U, typename F>
  static MappedMutexGuard<U> map(MutexGuard&& self, F&& f) __TA_NO_THREAD_SAFETY_ANALYSIS {
    auto* data = f(self.data_);
    return MappedMutexGuard<U>(ktl::move(self.take()), data);
  }

  Data* operator->() const { return data_; }
  Data* operator->() { return data_; }

  Data& operator*() const {
    DEBUG_ASSERT(data_ != nullptr);
    return *data_;
  }
  Data& operator*() {
    DEBUG_ASSERT(data_ != nullptr);
    return *data_;
  }

  ~MutexGuard() = default;

 private:
  DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(MutexGuard);
  Data* data_;
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

  RwLockReadGuard Read() const { return ktl::move(RwLockReadGuard(this)); }
  RwLockWriteGuard Write() { return ktl::move(RwLockWriteGuard(this)); }

 private:
  // No moving or copying allowed.
  DISALLOW_COPY_ASSIGN_AND_MOVE(RwLock);

  friend class RwLockGuard<Data, BrwLockPi::Reader>;
  friend class RwLockGuard<Data, BrwLockPi::Writer>;

  mutable DECLARE_BRWLOCK_PI(RwLock, lockdep::LockFlagsNone) lock_;
  Data data_ __TA_GUARDED(lock_);
};

template <typename Data, typename Option>
class RwLockGuard : public Guard<BrwLockPi, Option> {
 public:
  __WARN_UNUSED_CONSTRUCTOR explicit RwLockGuard(
      std::conditional_t<std::is_same_v<Option, BrwLockPi::Reader>, const RwLock<Data>*,
                         RwLock<Data>*>
          mtx)
      : Guard<BrwLockPi, Option>(&mtx->lock_), mtx_(mtx) {}

  RwLockGuard(RwLockGuard&& other) noexcept
      : Guard<BrwLockPi, Option>(AdoptLock, ktl::move(other.take())), mtx_(other.mtx_) {
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
  std::conditional_t<std::is_same_v<Option, BrwLockPi::Reader>, const RwLock<Data>*, RwLock<Data>*>
      mtx_;
};

}  // namespace starnix_sync

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_SYNC_INCLUDE_LIB_STARNIX_SYNC_LOCKS_H_

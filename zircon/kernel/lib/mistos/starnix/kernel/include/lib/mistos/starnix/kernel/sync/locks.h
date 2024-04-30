// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SYNC_LOCKS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SYNC_LOCKS_H_

#include <zircon/compiler.h>

#include <kernel/brwlock.h>
#include <kernel/lockdep.h>
#include <kernel/mutex.h>

namespace starnix {

template <typename Data>
class MutexGuard;

template <typename Data>
class StarnixMutex {
 public:
  StarnixMutex() = default;
  explicit StarnixMutex(Data&& data) : data_(data) {}

  // No moving or copying allowed.
  DISALLOW_COPY_ASSIGN_AND_MOVE(StarnixMutex);

  // Copy elision - Return value optimization (RVO)
  MutexGuard<Data> Lock() { return MutexGuard(this); }
  const MutexGuard<Data> Lock() const { return MutexGuard(this); }

 private:
  friend class MutexGuard<Data>;

  mutable DECLARE_MUTEX(StarnixMutex) lock_;
  Data data_ __TA_GUARDED(lock_);
};

template <typename Data>
class MutexGuard : public Guard<Mutex> {
 public:
  __WARN_UNUSED_CONSTRUCTOR explicit MutexGuard(StarnixMutex<Data>* mtx)
      : Guard(&mtx->lock_), mtx_(mtx) {}

  MutexGuard& operator=(const Data& data) __TA_NO_THREAD_SAFETY_ANALYSIS {
    mtx_->data_ = data;
    return *this;
  }

  Data* operator->() const __TA_ASSERT(mtx_->lock_.lock()) {
    DEBUG_ASSERT(mtx_->lock_.lock().IsHeld());
    return &mtx_->data_;
  }

  Data& operator*() const __TA_ASSERT(mtx_->lock_.lock()) {
    DEBUG_ASSERT(mtx_->lock_.lock().IsHeld());
    return mtx_->data_;
  }

 private:
  StarnixMutex<Data>* mtx_;
};

// template <typename Data>
// explicit MutexGuard(Data) -> MutexGuard<Data>;

template <typename Data, typename Option>
class RwLockGuard;

template <typename Data>
class RwLock {
 public:
  RwLock() = default;
  RwLock(Data&& data) : data_(data) {}

  // No moving or copying allowed.
  DISALLOW_COPY_ASSIGN_AND_MOVE(RwLock);

  // Copy elision - Return value optimization (RVO)
  RwLockGuard<Data, BrwLockPi::Reader> Read() { return RwLockGuard<Data, BrwLockPi::Reader>(this); }
  const RwLockGuard<Data, BrwLockPi::Reader> Read() const {
    return RwLockGuard<Data, BrwLockPi::Reader>(this);
  }

  // Copy elision - Return value optimization (RVO)
  RwLockGuard<Data, BrwLockPi::Writer> Write() {
    return RwLockGuard<Data, BrwLockPi::Writer>(this);
  }
  const RwLockGuard<Data, BrwLockPi::Writer> Write() const {
    return RwLockGuard<Data, BrwLockPi::Writer>(this);
  }

 private:
  friend class RwLockGuard<Data, BrwLockPi::Reader>;
  friend class RwLockGuard<Data, BrwLockPi::Writer>;

  mutable DECLARE_BRWLOCK_PI(RwLock, lockdep::LockFlagsMultiAcquire) lock_;
  Data data_ __TA_GUARDED(lock_);
};

template <typename Data, typename Option>
class RwLockGuard : public Guard<BrwLockPi, Option> {
 public:
  __WARN_UNUSED_CONSTRUCTOR explicit RwLockGuard(RwLock<Data>* mtx)
      : Guard<BrwLockPi, Option>(&mtx->lock_), mtx_(mtx) {}

  Data* operator->() const { return &mtx_->data_; }
  Data& operator*() const { return mtx_->data_; }

 private:
  RwLock<Data>* mtx_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SYNC_LOCKS_H_

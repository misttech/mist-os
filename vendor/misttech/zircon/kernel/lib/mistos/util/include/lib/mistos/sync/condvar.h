// Copyright 2025 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_SYNC_CONDVAR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_SYNC_CONDVAR_H_

#include <kernel/mutex.h>
#include <kernel/semaphore.h>
#include <ktl/atomic.h>

namespace sync {

class CondVar {
 public:
  CondVar() : waiters_(0) {}

  zx_status_t Wait(Mutex* mutex, Deadline deadline = Deadline::infinite()) __TA_REQUIRES(mutex) {
    waiters_.fetch_add(1, std::memory_order_relaxed);
    mutex->Release();
    zx_status_t status = sema_.Wait(deadline);
    mutex->Acquire();
    waiters_.fetch_sub(1, std::memory_order_relaxed);
    return status;
  }

  void Signal() {
    if (waiters_.load(std::memory_order_relaxed) > 0) {
      sema_.Post();
    }
  }

  void Broadcast() {
    int waiters = waiters_.load(std::memory_order_relaxed);
    for (int i = 0; i < waiters; i++) {
      sema_.Post();
    }
  }

 private:
  Semaphore sema_;
  ktl::atomic<int> waiters_;
};

}  // namespace sync
#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_SYNC_CONDVAR_H_

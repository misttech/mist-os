// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/compiler.h>

#include <mutex>

#include "thread-list.h"
#include "threads_impl.h"

extern "C" {

using LIBC_NAMESPACE::gAllThreads, LIBC_NAMESPACE::gAllThreadsLock;

// This is only called from the start path when there are no other threads.
// So it alone can access gAllThreads without hold gAllThreadsLock.
__TA_NO_THREAD_SAFETY_ANALYSIS void __thread_list_start(pthread* td) {
  assert(!gAllThreads);
  td->prevp = &gAllThreads;
  gAllThreads = td;
}

__TA_EXCLUDES(gAllThreadsLock) void __thread_list_add(pthread* td) {
  std::lock_guard lock(gAllThreadsLock);
  td->prevp = &gAllThreads;
  td->next = gAllThreads;
  if (td->next) {
    td->next->prevp = &td->next;
  }
  gAllThreads = td;
}

// A detached thread has to remove itself from the list.
// Joinable threads get removed only in pthread_join.
__TA_EXCLUDES(gAllThreadsLock) void __thread_list_erase(void* arg) {
  pthread* const t = static_cast<pthread*>(arg);
  std::lock_guard lock(gAllThreadsLock);
  *t->prevp = t->next;
  if (t->next) {
    t->next->prevp = t->prevp;
  }
}

}  // extern "C"

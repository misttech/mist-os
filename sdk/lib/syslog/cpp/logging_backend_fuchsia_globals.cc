// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/syslog/cpp/logging_backend_fuchsia_globals.h"

#ifndef _ALL_SOURCE
#define _ALL_SOURCE  // To get MTX_INIT
#endif

#include <zircon/assert.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <mutex>

#define EXPORT __attribute__((visibility("default")))

namespace {

syslog_runtime::internal::LogState* state = nullptr;
std::mutex state_lock;
// This thread's koid.
// Initialized on first use.
thread_local zx_koid_t tls_thread_koid{ZX_KOID_INVALID};

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

}  // namespace

extern "C" {

EXPORT
zx_koid_t FuchsiaLogGetCurrentThreadKoid() {
  if (unlikely(tls_thread_koid == ZX_KOID_INVALID)) {
    tls_thread_koid = GetKoid(zx_thread_self());
  }
  ZX_DEBUG_ASSERT(tls_thread_koid != ZX_KOID_INVALID);
  return tls_thread_koid;
}

EXPORT
void FuchsiaLogSetStateLocked(syslog_runtime::internal::LogState* new_state) { state = new_state; }

EXPORT void FuchsiaLogAcquireState() __TA_NO_THREAD_SAFETY_ANALYSIS { return state_lock.lock(); }

EXPORT void FuchsiaLogReleaseState() __TA_NO_THREAD_SAFETY_ANALYSIS { return state_lock.unlock(); }

EXPORT
syslog_runtime::internal::LogState* FuchsiaLogGetStateLocked() { return state; }

}  // extern "C"

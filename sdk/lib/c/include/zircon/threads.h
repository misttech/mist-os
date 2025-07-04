// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_THREADS_H_
#define ZIRCON_THREADS_H_

#include <threads.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

__BEGIN_CDECLS

// Get the zx_handle_t corresponding to the thrd_t. This handle is still owned by the C11 thread,
// and will not persist after the thread exits. Callers must duplicate the handle, therefore, if
// they wish the thread handle to outlive the execution of the C11 thread.
zx_handle_t thrd_get_zx_handle(thrd_t t);

// Sets the process handle that is used, by this thread, when creating new threads.
//
// This does not take ownership of the passed in handle, and does not transfer ownership of the old
// handle. The caller is responsible for ensuring that the handle remains valid for the life of the
// thread, or until a different handle is installed.
//
// The default value is copied from zx_process_self() in the initial thread, and then copied from
// the thread calling thrd_create into each new thread.
//
// Returns the previously set process handle.
zx_handle_t thrd_set_zx_process(zx_handle_t proc_handle);

// Returns the same handle as thrd_set_zx_process without modifying the calling thread's process
// handle.
//
// This does not transfer ownership of the current handle. The caller is responsible for only using
// the returned handle while it remains valid.
zx_handle_t thrd_get_zx_process(void);

// Converts a threads.h-style status value to an |zx_status_t|.
static inline zx_status_t __PURE thrd_status_to_zx_status(int thrd_status) {
  switch (thrd_status) {
    case thrd_success:
      return ZX_OK;
    case thrd_nomem:
      return ZX_ERR_NO_MEMORY;
    case thrd_timedout:
      return ZX_ERR_TIMED_OUT;
    case thrd_busy:
      return ZX_ERR_SHOULD_WAIT;
    default:
    case thrd_error:
      return ZX_ERR_INTERNAL;
  }
}

__END_CDECLS

#ifdef __cplusplus

#if __has_include(<thread>)

#include <thread>

// Get the zx_handle_t corresponding to the std::thread::native_handle() value.
// See `thrd_get_zx_handle` (above) for constraints on the returned handle.
// Using this API avoids any assumptions about std::thread::native_handle_type
// corresponding exactly to thrd_t or any other particular type.
extern "C" zx_handle_t native_thread_get_zx_handle(std::thread::native_handle_type);

#endif  // __has_include(<thread>)

#endif  // __cplusplus

#endif  // ZIRCON_THREADS_H_

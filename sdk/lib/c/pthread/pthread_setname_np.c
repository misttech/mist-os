// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string.h>
#include <zircon/compiler.h>
#include <zircon/status.h>

#include "libc.h"
#include "threads_impl.h"

__EXPORT int pthread_setname_np(pthread_t thread, const char *name) {
  zx_handle_t handle = zxr_thread_get_handle(&thread->zxr_thread);
  zx_status_t status = zx_object_set_property(handle, ZX_PROP_NAME, name, strlen(name));
  switch (status) {
    case ZX_OK:
      return 0;
    // The thread has exited but has not been joined so the internal thread object has died but not
    // this pthread_t.
    case ZX_ERR_BAD_STATE:
      return ESRCH;
    default:
      ZX_PANIC("unexpected status '%s'", zx_status_get_string(status));
  }
}

// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/syscalls/forward.h>
#include <lib/syscalls/safe-syscall-argument.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>

#include "../priv.h"

#define LOCAL_TRACE SYSCALLS_GLOBAL_TRACE(0)

long sys_read(unsigned int fd, user_out_ptr<char> buf, size_t count) {
  LTRACEF_LEVEL(2, "fd=%d count=%zu\n", fd, count);
  return -1;
}

static zx_status_t do_write_to_log(user_in_ptr<const char> ptr, size_t count) {
#if 0
  char buf[1024];
  if (ptr.copy_array_from_user(buf, count) != ZX_OK)
    return ZX_ERR_INVALID_ARGS;

  // Dump what we can into the persistent dlog, if we have one.
  persistent_dlog_write({buf, count});

  // This path to serial out arbitrates with the debug log
  // drainer and/or kernel ll debug path to minimize interleaving
  // of serial output between various sources
  dlog_serial_write({buf, count});

  return ZX_OK;
#endif

  char buf[256];
  while (count != 0) {
    size_t i;
    size_t chunk = count;
    if (chunk > 256)
      chunk = 256;

    if (ptr.copy_array_from_user(buf, chunk) != ZX_OK)
      return ZX_ERR_INVALID_ARGS;

    for (i = 0; i < chunk; i++) {
      fputc(buf[i], stdout);
    }
    ptr = ptr.byte_offset(chunk);
    count -= chunk;
  }

  return ZX_OK;
}

long sys_write(unsigned int fd, user_in_ptr<const char> ptr, size_t count) {
  LTRACEF_LEVEL(2, "fd=%d count=%zu\n", fd, count);
  if (fd == 1) {
    zx_status_t status = do_write_to_log(ptr, count);
    if (status != ZX_OK) {
      return -EBADF;
    }
    return count;
  }
  return 0;
}

long sys_open(user_in_ptr<const char> filename, int flags, umode_t mode) {
  LTRACEF_LEVEL(2, "\n");
  return -1;
}

long sys_close(unsigned int fd) {
  LTRACEF_LEVEL(2, "fd=%d\n", fd);
  return -1;
}

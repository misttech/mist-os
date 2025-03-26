// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_LINUX_UAPI_STUB_BITS_FLOCK_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_STUB_BITS_FLOCK_H_

#include <asm/posix_types.h>

struct flock {
  short l_type;            /* lock type: read/write, etc. */
  short l_whence;          /* type of l_start */
  __kernel_loff_t l_start; /* starting offset */
  __kernel_loff_t l_len;   /* len = 0 means until end of file */
  pid_t l_pid;             /* lock owner */
};

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_STUB_BITS_FLOCK_H_

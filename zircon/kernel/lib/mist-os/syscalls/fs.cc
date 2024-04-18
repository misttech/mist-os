// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/syscalls/forward.h>
#include <lib/syscalls/safe-syscall-argument.h>
#include <lib/user_copy/user_ptr.h>

// clang format off
#include <lib/mistos/linux_uapi/typedefs.h>
// clang format on

long sys_read(unsigned int fd, user_out_ptr<char> buf, size_t count) { return -1; }
long sys_write(unsigned int fd, user_in_ptr<const char> ptr, size_t count) { return -1; }
long sys_open(user_in_ptr<const char> filename, int flags, umode_t mode) { return -1; }
long sys_close(unsigned int fd) { return -1; }

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_LINUX_UAPI_STUB_SYS_SOCKET_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_STUB_SYS_SOCKET_H_

#include <stdint.h>

#include <asm/posix_types.h>

#define AF_UNSPEC 0
#define AF_INET 2
#define AF_INET6 10

typedef uint32_t socklen_t;

struct ucred {
  __kernel_pid_t pid;
  __kernel_uid_t uid;
  __kernel_gid_t gid;
};

struct msghdr {
  void* msg_name;
  socklen_t msg_namelen;
  struct iovec* msg_iov;
  size_t msg_iovlen;
  void* msg_control;
  size_t msg_controllen;
  unsigned int msg_flags;
};

struct cmsghdr {
  size_t cmsg_len;
  unsigned int cmsg_level;
  unsigned int cmsg_type;
};

struct mmsghdr {
  struct msghdr msg_hdr;
  unsigned int msg_len;
};

struct linger {
  int l_onoff;
  int l_linger;
};

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_STUB_SYS_SOCKET_H_

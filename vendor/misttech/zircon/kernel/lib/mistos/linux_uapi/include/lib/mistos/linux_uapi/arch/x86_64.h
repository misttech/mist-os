// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_LINUX_UAPI_INCLUDE_LIB_MISTOS_LINUX_UAPI_ARCH_X86_64_H_
#define ZIRCON_KERNEL_LIB_MISTOS_LINUX_UAPI_INCLUDE_LIB_MISTOS_LINUX_UAPI_ARCH_X86_64_H_

#include <inttypes.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <linux/aio_abi.h>
#include <linux/capability.h>
#include <linux/fs.h>
#include <linux/signal.h>
#include <linux/socket.h>
#include <linux/types.h>

enum landlock_rule_type : unsigned int;

typedef __s8 s8;
typedef __u8 u8;
typedef __s16 s16;
typedef __u16 u16;
typedef __s32 s32;
typedef __u32 u32;
typedef __s64 s64;
typedef __u64 u64;

typedef __kernel_fd_set fd_set;
typedef __kernel_ulong_t ino_t;
typedef __kernel_mode_t mode_t;
typedef unsigned short umode_t;
typedef u32 nlink_t;
typedef __kernel_off_t off_t;
typedef __kernel_pid_t pid_t;
typedef __kernel_daddr_t daddr_t;
typedef __kernel_key_t key_t;
typedef __kernel_suseconds_t suseconds_t;
typedef __kernel_timer_t timer_t;
typedef __kernel_clockid_t clockid_t;
typedef __kernel_mqd_t mqd_t;
typedef __kernel_uid32_t uid_t;
typedef __kernel_gid32_t gid_t;
typedef __kernel_uid16_t uid16_t;
typedef __kernel_gid16_t gid16_t;
typedef __kernel_loff_t loff_t;
typedef __kernel_rwf_t rwf_t;
typedef int32_t key_serial_t;
typedef __kernel_uid32_t qid_t; /* Type in which we store ids in memory */

struct linux_dirent64 {
  u64 d_ino;
  s64 d_off;
  unsigned short d_reclen;
  unsigned char d_type;
  char d_name[];
};

struct k_sigaction {
  struct sigaction sa;
#ifdef __ARCH_HAS_KA_RESTORER
  __sigrestore_t ka_restorer;
#endif
};

struct sockaddr {
  __kernel_sa_family_t sa_family;
  char sa_data[14];
};

typedef struct __kernel_sockaddr_storage sockaddr_storage;
typedef __kernel_sa_family_t sa_family_t;

#endif  // ZIRCON_KERNEL_LIB_MISTOS_LINUX_UAPI_INCLUDE_LIB_MISTOS_LINUX_UAPI_ARCH_X86_64_H_

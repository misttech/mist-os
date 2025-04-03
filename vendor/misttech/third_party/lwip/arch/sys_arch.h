// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_THIRD_PARTY_LWIP_ARCH_SYS_ARCH_H_
#define VENDOR_MISTTECH_THIRD_PARTY_LWIP_ARCH_SYS_ARCH_H_

#include <stdint.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

#define SYS_MBOX_NULL NULL
#define SYS_SEM_NULL NULL

typedef uint64_t sys_prot_t;

struct sys_sem;
typedef struct sys_sem* sys_sem_t;

struct sys_mutex;
typedef struct sys_mutex* sys_mutex_t;

struct sys_mbox;
typedef struct sys_mbox* sys_mbox_t;

struct Thread;
typedef struct Thread* sys_thread_t;

extern int* __geterrno(void);
#define errno (*__geterrno())

#define SYS_ARCH_INC(var, val) __atomic_fetch_add(&var, (val), __ATOMIC_SEQ_CST);
#define SYS_ARCH_DEC(var, val) __atomic_fetch_sub(&var, (val), __ATOMIC_SEQ_CST)
#define SYS_ARCH_GET(var, ret)                     \
  do {                                             \
    ret = __atomic_load_n(&var, __ATOMIC_SEQ_CST); \
  } while (0)
#define SYS_ARCH_SET(var, val) __atomic_store_n(&var, (val), __ATOMIC_SEQ_CST)

__END_CDECLS

#endif  // VENDOR_MISTTECH_THIRD_PARTY_LWIP_ARCH_SYS_ARCH_H_

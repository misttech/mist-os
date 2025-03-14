// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_TYPES_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_TYPES_H_

#include <limits.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>
#include <zircon/types.h>

typedef unsigned char uchar;
typedef unsigned short ushort;
typedef unsigned int uint;
typedef unsigned long ulong;
typedef unsigned char u_char;
typedef unsigned short u_short;
typedef unsigned int u_int;
typedef unsigned long u_long;

typedef long long off_t;

typedef zx_status_t status_t;

typedef uintptr_t addr_t;
typedef uintptr_t vaddr_t;
typedef uintptr_t paddr_t;

typedef int kobj_id;

typedef zx_time_t lk_time_t;
typedef zx_time_t lk_bigtime_t;
#define INFINITE_TIME ZX_TIME_INFINITE

#define TIME_GTE(a, b) ((int64_t)((a) - (b)) >= 0)
#define TIME_LTE(a, b) ((int64_t)((a) - (b)) <= 0)
#define TIME_GT(a, b) ((int64_t)((a) - (b)) > 0)
#define TIME_LT(a, b) ((int64_t)((a) - (b)) < 0)

enum handler_return {
  INT_NO_RESCHEDULE = 0,
  INT_RESCHEDULE,
};

typedef signed long int ssize_t;

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;

typedef int8_t s8;
typedef int16_t s16;
typedef int32_t s32;
typedef int64_t s64;

typedef uint8_t u_int8_t;
typedef uint16_t u_int16_t;
typedef uint32_t u_int32_t;
typedef uint64_t u_int64_t;

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_TYPES_H_

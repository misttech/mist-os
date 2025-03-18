// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_THIRD_PARTY_LWIP_ARCH_CC_H_
#define VENDOR_MISTTECH_THIRD_PARTY_LWIP_ARCH_CC_H_

#include <zircon/assert.h>
#include <zircon/compiler.h>

#define LWIP_NO_UNISTD_H 1

#define LWIP_RAND() ((u32_t)rand())

#define LWIP_PLATFORM_ASSERT(x) ZX_ASSERT_MSG(true, x)

//#define LWIP_CHKSUM_ALGORITHM 2

#define PACK_STRUCT_STRUCT __PACKED

#endif  // VENDOR_MISTTECH_THIRD_PARTY_LWIP_ARCH_CC_H_

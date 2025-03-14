// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_COMPILER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_COMPILER_H_

#include <zircon/compiler.h>

#define __UNUSED __attribute__((__unused__))

#if !defined(countof)
#define countof(a) (sizeof(a) / sizeof((a)[0]))
#endif

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_COMPILER_H_

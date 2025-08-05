// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_INCLUDE_ALIGN_H_
#define ZIRCON_KERNEL_INCLUDE_ALIGN_H_

#include <arch/defines.h>

#define ROUNDUP(a, b) (((a) + ((b) - 1)) & ~((b) - 1))
#define ROUNDDOWN(a, b) ((a) & ~((b) - 1))

#define IS_ROUNDED(a, b) (!(((uintptr_t)(a)) & (((uintptr_t)(b)) - 1)))

#define ROUNDUP_PAGE_SIZE(x) ROUNDUP((x), PAGE_SIZE)
#define ROUNDDOWN_PAGE_SIZE(x) ROUNDDOWN((x), PAGE_SIZE)

#define IS_PAGE_ROUNDED(x) IS_ROUNDED((x), PAGE_SIZE)

#endif  // ZIRCON_KERNEL_INCLUDE_ALIGN_H_

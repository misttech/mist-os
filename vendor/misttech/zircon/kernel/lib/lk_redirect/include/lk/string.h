/*
 * Copyright (c) 2008 Travis Geiselbrecht
 *
 * Use of this source code is governed by a MIT-style
 * license that can be found in the LICENSE file or at
 * https://opensource.org/licenses/MIT
 */

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_STRING_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_STRING_H_

#include <stddef.h>
#include <zircon/compiler.h>

__BEGIN_CDECLS

char *strdup(const char *str) __MALLOC;

__END_CDECLS

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_LK_REDIRECT_INCLUDE_LK_STRING_H_

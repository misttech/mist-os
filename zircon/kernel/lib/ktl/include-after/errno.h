// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_KTL_INCLUDE_ERRNO_H_
#define ZIRCON_KERNEL_LIB_KTL_INCLUDE_ERRNO_H_

// The kernel doesn't want this file but some libc++ headers we need
// wind up including it.

#if __mist_os__
#if __has_include_next(<errno.h>)
#include_next <errno.h>
#endif
#endif

#endif  // ZIRCON_KERNEL_LIB_KTL_INCLUDE_ERRNO_H_

// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_KTL_INCLUDE___CHRONO_DURATION_H_
#define ZIRCON_KERNEL_LIB_KTL_INCLUDE___CHRONO_DURATION_H_

// libc++'s <atomic> includes this file via <chrono> via <__threading_support>.
// The real libc++ implementation contains pieces that are not kernel-friendly.
// To wit, they declare inline function returning floating-point types, which
// GCC does not allow when floating-point code generation is disabled.
//
// The problematic portions of <chrono> only in the <__chrono/duration.h>
// implementation header and there are inside `#if _LIBCPP_STD_VER > 11`
// conditionals.  The subset of the <chrono> API required for <atomic> does not
// include anything post-C++11.  So the kernel can use the libc++ header if it
// sees `_LIBCPP_STD_VER` as 11.  However, the other headers included by
// <__chrono/duration.h> need to declare their full APIs.  So first `#include`
// all those directly so they are evaluated with the normal `_LIBCPP_STD_VER`
// setting.  Then momentarily redefine `_LIBCPP_STD_VER` while including
// <__chrono/duration.h> itself.

#include <__config>
#include <limits>
#include <ratio>
#include <type_traits>

#pragma push_macro("_LIBCPP_STD_VER")
#undef _LIBCPP_STD_VER
#define _LIBCPP_STD_VER 11

#include_next <__chrono/duration.h>

#pragma pop_macro("_LIBCPP_STD_VER")

#endif  // ZIRCON_KERNEL_LIB_KTL_INCLUDE___CHRONO_DURATION_H_

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC___SUPPORT_MACROS_ATTRIBUTES_H_
#define SRC___SUPPORT_MACROS_ATTRIBUTES_H_

// This interposes on the llvm-libc "src/__support/macros/attributes.h" header
// to redefine the LIBC_INLINE macro.

#include_next "src/__support/macros/attributes.h"

#undef LIBC_INLINE

// If an inline function doesn't get inlined, it will go into a COMDAT group.
// There it will be de-dup'd with any definitions from other translation units.
// Since those might not have been in the user.basic environment, they might
// include sanitizer instrumentation, shadow-call-stack, etc. To ensure that
// the functions compiled specially in the user.basic environment do not have
// any cross-pollination with other translation units, make sure every inline
// function is defined with internal linkage rather than in a COMDAT group.
#ifdef __clang__
#define LIBC_INLINE [[clang::internal_linkage]] inline
#else
// GCC doesn't have an attribute for internal linkage, so the best available is
// to try to force inlining. (`static` can't be used here because this is also
// applied to member functions.)
#define LIBC_INLINE [[gnu::always_inline]] inline
#endif

#endif  // SRC___SUPPORT_MACROS_ATTRIBUTES_H_

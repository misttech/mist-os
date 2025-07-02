// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef PREEMPT_SRC___SUPPORT_UINT128_H_
#define PREEMPT_SRC___SUPPORT_UINT128_H_

// TODO(https://fxbug.dev/42105189): These are defined as macros in
// <zircon/compiler.h> and used by some other headers such as in libzx.  This
// conflicts with their use as scoped identifiers in the llvm-libc code reached
// from this header, when this header is included in someplace that also
// includes <zircon/compiler.h> and those headers that rely on its macros.
// <zircon/compiler.h> should not be defining macros in the public namespace
// this way, but until that's fixed work around the issue by hiding the macros
// during the evaluation of the llvm-libc headers.
#pragma push_macro("add_overflow")
#undef add_overflow
#pragma push_macro("sub_overflow")
#undef sub_overflow

#include_next "src/__support/uint128.h"

// TODO(https://fxbug.dev/42105189): See comment above.
#pragma pop_macro("add_overflow")
#pragma pop_macro("sub_overflow")

#endif  // PREEMPT_SRC___SUPPORT_UINT128_H_

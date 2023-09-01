// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Bindgen doesn't currently support automatically translating C++ atomic
// types into corresponding rust types.
// Below is workaround where atomic variables are defined as special
// placeholder types with the same size and alignment as the real ones.
// generate.py recognises these types and replaces them with the rust
// equivalent
#ifndef SRC_STARNIX_LIB_LINUX_UAPI_BINDGEN_ATOMICS_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_BINDGEN_ATOMICS_H_

#ifdef IS_BINDGEN
#define Atomic(t) \
  struct {        \
    t v;          \
  }
#else
// Normal atomics are used when the file isn't being used to generate
// rust bindings
#include <atomic>
#define Atomic(t) std::atomic<t>
#endif

typedef Atomic(int64_t) StdAtomicI64;
typedef Atomic(uint32_t) StdAtomicU32;

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_BINDGEN_ATOMICS_H_

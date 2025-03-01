// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_TLSDESC_RUNTIME_DYNAMIC_H_
#define LIB_DL_TLSDESC_RUNTIME_DYNAMIC_H_

#include <lib/elfldltl/layout.h>
#include <lib/ld/tlsdesc.h>

#include <cstddef>

namespace [[gnu::visibility("hidden")]] dl {

// The argument to TLSDESC hooks is `const TlsDescGot&`.
using TlsDescGot = elfldltl::Elf<>::TlsDescGot<>;

// For dynamic TLS modules, each thread's copy of each dynamic PT_TLS segment
// is found in by index into an array of pointers.  That array itself is found
// as a normal thread_local variable dl::_dl_tlsdesc_runtime_dynamic_blocks
// (owned by libdl) using a normal IE access These TLSDESC hooks take two
// values: an index into that array, and an offset within that PT_TLS segment.
// They compute `_dl_tlsdesc_runtime_dynamic_blocks[index] + offset - $tp`.
//
// The TLSDESC ABI provides for one address-sized word to encode the argument
// to the TLSDESC hook (TlsDescGot::value).  There are two TLSDESC hooks that
// encode those two values in different ways:
//
//  * The "split" version encodes both values directly in the word by splitting
//    it in half bitwise.  The index is found in the high bits.  The offset is
//    found in the low bits.  This version is used whenever each value fits
//    into half the bits of the word.
//
//  * The "indirect" version uses the word as a pointer to an allocated data
//    structure containing the index and offset (TlsdescIndirect).
//
// **NOTE:** There is no special provision for synchronization so far.  These
// entry points assume that the current thread's blocks vector pointer is valid
// for any index they can be passed.

struct TlsdescIndirect {
  size_t index, offset;
};

// These are used or defined in assembly (see tlsdesc-runtime-dynamic.S), so
// they need unmangled linkage names.  From C++, they're still namespaced.
extern "C" {

// The runtime hooks access `_dl_tlsdesc_runtime_dynamic_blocks[index]`.
extern constinit thread_local std::byte** _dl_tlsdesc_runtime_dynamic_blocks
    [[gnu::tls_model("initial-exec")]];

// This hook splits the `value` field in half, with index in the high bits.
extern ld::TlsdescCallback _dl_tlsdesc_runtime_dynamic_split;

// This hook makes the `value` field a `const TlsdescIndirect*`.
extern ld::TlsdescCallback _dl_tlsdesc_runtime_dynamic_indirect;

}  // extern "C"

}  // namespace dl

#endif  // LIB_DL_TLSDESC_RUNTIME_DYNAMIC_H_

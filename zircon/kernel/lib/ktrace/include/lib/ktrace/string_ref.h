// Copyright 2019 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_KTRACE_INCLUDE_LIB_KTRACE_STRING_REF_H_
#define ZIRCON_KERNEL_LIB_KTRACE_INCLUDE_LIB_KTRACE_STRING_REF_H_

#include <lib/fxt/interned_string.h>

template <typename T, T... chars>
inline fxt::InternedString* operator""_stringref() {
  return const_cast<fxt::InternedString*>(&fxt::operator""_intern<T, chars... >());
}

using StringRef = fxt::InternedString;

#endif  // ZIRCON_KERNEL_LIB_KTRACE_INCLUDE_LIB_KTRACE_STRING_REF_H_

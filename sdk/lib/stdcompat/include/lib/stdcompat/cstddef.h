// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_STDCOMPAT_CSTDDEF_H_
#define LIB_STDCOMPAT_CSTDDEF_H_

#include <cstddef>
#include <type_traits>

#include "version.h"

namespace cpp17 {

using std::byte;
using std::operator<<=;
using std::operator<<;
using std::operator>>=;
using std::operator>>;
using std::operator|=;
using std::operator|;
using std::operator&=;
using std::operator&;
using std::operator^=;
using std::operator^;
using std::operator~;
using std::to_integer;

}  // namespace cpp17

#endif  // LIB_STDCOMPAT_CSTDDEF_H_

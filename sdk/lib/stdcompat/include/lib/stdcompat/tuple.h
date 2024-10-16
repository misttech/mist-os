// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_STDCOMPAT_TUPLE_H_
#define LIB_STDCOMPAT_TUPLE_H_

#include <cstddef>
#include <tuple>

#include "functional.h"

namespace cpp17 {

using std::tuple_size_v;

using std::apply;

}  // namespace cpp17

#endif  // LIB_STDCOMPAT_TUPLE_H_

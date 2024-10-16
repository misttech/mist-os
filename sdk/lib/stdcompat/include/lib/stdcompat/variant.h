// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_STDCOMPAT_VARIANT_H_
#define LIB_STDCOMPAT_VARIANT_H_

#include <cstddef>
#include <variant>

#include "array.h"
#include "functional.h"
#include "internal/exception.h"
#include "type_traits.h"
#include "utility.h"

namespace cpp17 {

using std::bad_variant_access;
using std::get;
using std::holds_alternative;
using std::monostate;
using std::variant;
using std::variant_alternative;
using std::variant_alternative_t;
using std::variant_size;
using std::variant_size_v;
using std::visit;

}  // namespace cpp17

#endif  // LIB_STDCOMPAT_VARIANT_H_

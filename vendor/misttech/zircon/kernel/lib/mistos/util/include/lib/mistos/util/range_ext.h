// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_RANGE_EXT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_RANGE_EXT_H_

#include <lib/mistos/util/range-map.h>

#include <ktl/algorithm.h>

namespace util {

/// Returns the intersection of lhs and rhs. If there is no intersection, the result will
/// return true for .is_empty().
template <typename T>
Range<T> intersect(Range<T> lhs, Range<T> rhs) {
  return Range<T>{.start = ktl::max(lhs.start, rhs.start), .end = ktl::min(lhs.end, rhs.end)};
}

}  // namespace util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_RANGE_EXT_H_

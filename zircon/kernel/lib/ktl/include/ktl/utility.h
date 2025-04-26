// Copyright 2018 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_KTL_INCLUDE_KTL_UTILITY_H_
#define ZIRCON_KERNEL_LIB_KTL_INCLUDE_KTL_UTILITY_H_

#include <utility>

namespace ktl {

using std::exchange;
using std::forward;
using std::move;
using std::swap;

using std::in_place;
using std::in_place_index;
using std::in_place_index_t;
using std::in_place_t;

using std::index_sequence;
using std::index_sequence_for;
using std::integer_sequence;
using std::make_index_sequence;
using std::make_integer_sequence;

}  // namespace ktl

#endif  // ZIRCON_KERNEL_LIB_KTL_INCLUDE_KTL_UTILITY_H_

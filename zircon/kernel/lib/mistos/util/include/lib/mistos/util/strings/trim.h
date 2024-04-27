// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STRINGS_TRIM_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STRINGS_TRIM_H_

#include <string_view>

namespace util {

// Returns a std::string_view over str, where chars_to_trim are removed from the
// beginning and end of the std::string_view.
std::string_view TrimString(std::string_view str, std::string_view chars_to_trim);

}  // namespace util

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STRINGS_TRIM_H_

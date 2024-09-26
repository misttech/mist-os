// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_DEFAULT_CONSTRUCT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_DEFAULT_CONSTRUCT_H_

namespace mtl {

template <typename T>
T DefaultConstruct() {
  return {};
}

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_DEFAULT_CONSTRUCT_H_

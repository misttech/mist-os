// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BTREE_MAP_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BTREE_MAP_H_

#include <lib/mistos/util/allocator.h>

#include <map>

namespace util {

template <typename _KeyType, typename _ValueType, typename Compare = std::less<_KeyType>>
class BTreeMap : public std::map<_KeyType, _ValueType, Compare,
                                 util::Allocator<std::pair<const _KeyType, _ValueType>>> {
 public:
  using KeyType = _KeyType;
  using ValueType = _ValueType;
  using Base =
      std::map<KeyType, ValueType, Compare, util::Allocator<std::pair<const KeyType, ValueType>>>;

  explicit BTreeMap(const util::Allocator<std::pair<const KeyType, ValueType>>& alloc =
                        util::Allocator<std::pair<const KeyType, ValueType>>())
      : Base(Compare(), alloc) {}
};

}  // namespace util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BTREE_MAP_H_

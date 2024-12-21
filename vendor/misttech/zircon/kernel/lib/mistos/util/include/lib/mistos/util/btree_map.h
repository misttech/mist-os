// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BTREE_MAP_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BTREE_MAP_H_

#include <lib/mistos/util/allocator.h>

#include <map>

namespace util {

// Define the Bound class
template <typename T>
class Bound {
 public:
  enum class Type { Included, Excluded, Unbounded };

  // Factory methods
  static Bound Included(const T& value) { return Bound(Type::Included, value); }
  static Bound Excluded(const T& value) { return Bound(Type::Excluded, value); }
  static Bound Unbounded() { return Bound(); }

  Bound() : type_(Type::Unbounded), value_(std::nullopt) {}

  Type type() const { return type_; }
  const std::optional<T>& value() const { return value_; }

 private:
  Bound(Type type, std::optional<T> value) : type_(type), value_(value) {}

  Type type_;
  std::optional<T> value_;
};

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

  // Range query
  auto range(const Bound<KeyType>& lower, const Bound<KeyType>& upper) const {
    auto begin = get_lower_bound(lower);
    auto end = get_upper_bound(upper);
    return std::make_pair(begin, end);
  }

 private:
  typename Base::const_iterator get_lower_bound(const Bound<KeyType>& bound) const {
    if (bound.type() == Bound<KeyType>::Type::Unbounded) {
      return this->begin();
    }
    const auto& value = bound.value();
    return bound.type() == Bound<KeyType>::Type::Included ? this->lower_bound(*value)
                                                          : this->upper_bound(*value);
  }

  typename Base::const_iterator get_upper_bound(const Bound<KeyType>& bound) const {
    if (bound.type() == Bound<KeyType>::Type::Unbounded) {
      return this->end();
    }
    const auto& value = bound.value();
    return bound.type() == Bound<KeyType>::Type::Included ? this->upper_bound(*value)
                                                          : this->lower_bound(*value);
  }
};

}  // namespace util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BTREE_MAP_H_

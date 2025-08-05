// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ID_TYPE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ID_TYPE_H_

#include <functional>
#include <type_traits>

namespace display::internal {

// `IdType` traits implementation that works for most display types.
//
// `ValueT` is the underlying type for the integer ID. Only C++ integer types are
// supported.
//
// `FidlT` is the FIDL type used to represent IDs. It must be a wire type for a
// FIDL struct whose `value` member is the same integer type as `ValueT`.
//
// `BanjoT` is the Banjo type used to represent IDs. It must be the same integer
// type as `ValueT`, or `std::false_type`. `std::false_type` means that the ID is
// not represented in Banjo.
template <typename ValueT, typename FidlT, typename BanjoT>
struct DefaultIdTypeTraits {
  // The ID's underlying type.
  using ValueType = ValueT;

  // The corresponding FIDL wire type.
  using FidlType = FidlT;

  // The corresponding Banjo type.
  //
  // `std::false_type` if there is no corresponding Banjo type.
  using BanjoType = BanjoT;

  // Converts from the FIDL wire type to the underlying type.
  static constexpr ValueType FromFidl(const FidlType& fidl_value) noexcept;

  // Converts from the Banjo type to the underlying type.
  template <typename _ = std::enable_if<!std::is_same_v<BanjoType, std::false_type>>>
  static constexpr ValueType FromBanjo(const BanjoType& banjo_value) noexcept;

  // Converts from the underlying type to the FIDL wire type.
  static constexpr FidlType ToFidl(const ValueType& value) noexcept;

  // Converts from the underlying type to the Banjo type.
  template <typename _ = std::enable_if<!std::is_same_v<BanjoType, std::false_type>>>
  static constexpr BanjoType ToBanjo(const ValueType& value) noexcept;
};

// Newtype pattern implementation for integer identifiers.
//
// Instances are value types, and support being stored in containers. Copying
// is supported. The destructor is trivial.
//
// Instances support incrementing for sequential ID generation.
//
// Instances support being used as container keys, by implementing comparison with
// strict ordering guarantees and a std::hash specialization.
//
// This type is a zero-cost abstraction. Compiler optimizations will remove any
// overhead associated with it.
//
// `IdTraits` must be a traits structure with the same type aliases and
// static methods as `DefaultIdTraits`.
template <typename IdTraits>
class IdType {
 public:
  using ValueType = typename IdTraits::ValueType;
  using FidlType = typename IdTraits::FidlType;
  using BanjoType = typename IdTraits::BanjoType;

  constexpr IdType() noexcept = default;

  constexpr explicit IdType(const ValueType& int_value) noexcept : value_(int_value) {}

  template <typename _ = std::enable_if<!std::is_same_v<FidlType, ValueType>>>
  constexpr explicit IdType(const FidlType& fidl_value) noexcept
      : value_(IdTraits::FromFidl(fidl_value)) {}

  template <typename _ = std::enable_if<!std::is_same_v<BanjoType, ValueType> &&
                                        !std::is_same_v<BanjoType, std::false_type>>>
  constexpr explicit IdType(const BanjoType& banjo_value) noexcept
      : value_(IdTraits::FromBanjo(banjo_value)) {}

  constexpr IdType(const IdType&) noexcept = default;
  constexpr IdType(IdType&&) noexcept = default;
  constexpr IdType& operator=(const IdType&) noexcept = default;
  constexpr IdType& operator=(IdType&&) noexcept = default;

  ~IdType() = default;

  constexpr explicit operator ValueType() const { return value_; }
  constexpr const ValueType& value() const { return value_; }

  constexpr FidlType ToFidl() const { return IdTraits::ToFidl(value_); }

  template <typename _ = std::enable_if<!std::is_same_v<BanjoType, std::false_type>>>
  constexpr BanjoType ToBanjo() const {
    return IdTraits::ToBanjo(value_);
  }

  constexpr IdType& operator++();
  constexpr IdType operator++(int);

 private:
  ValueType value_;
};

// Equality comparison can be defaulted when C++20 is required.
template <typename IdTraits>
constexpr bool operator==(const IdType<IdTraits>& lhs, const IdType<IdTraits>& rhs) {
  return lhs.value() == rhs.value();
}
template <typename IdTraits>
constexpr bool operator!=(const IdType<IdTraits>& lhs, const IdType<IdTraits>& rhs) {
  return !(lhs == rhs);
}

// Ordering comparison can be defaulted when C++20 is required.
template <typename IdTraits>
constexpr bool operator<(const IdType<IdTraits>& lhs, const IdType<IdTraits>& rhs) {
  return lhs.value() < rhs.value();
}
template <typename IdTraits>
constexpr bool operator>=(const IdType<IdTraits>& lhs, const IdType<IdTraits>& rhs) {
  return !(lhs < rhs);
}
template <typename IdTraits>
constexpr bool operator>(const IdType<IdTraits>& lhs, const IdType<IdTraits>& rhs) {
  return lhs.value() > rhs.value();
}
template <typename IdTraits>
constexpr bool operator<=(const IdType<IdTraits>& lhs, const IdType<IdTraits>& rhs) {
  return !(lhs > rhs);
}

template <typename IdTraits>
constexpr IdType<IdTraits>& IdType<IdTraits>::operator++() {
  ++value_;
  return *this;
}

template <typename IdTraits>
constexpr IdType<IdTraits> IdType<IdTraits>::operator++(int) {
  const IdType return_value = *this;
  ++value_;
  return return_value;
}

template <typename ValueT, typename FidlT, typename BanjoT>
constexpr ValueT DefaultIdTypeTraits<ValueT, FidlT, BanjoT>::FromFidl(
    const FidlT& fidl_value) noexcept {
  static_assert(std::is_same_v<decltype(FidlT::value), ValueT>,
                "If the FIDL struct's value type does not match the underlying integer type, use "
                "custom traits, override FromFidl(), and document the safety of a static_cast");

  return fidl_value.value;
}

template <typename ValueT, typename FidlT, typename BanjoT>
template <typename _>
constexpr ValueT DefaultIdTypeTraits<ValueT, FidlT, BanjoT>::FromBanjo(
    const BanjoT& banjo_value) noexcept {
  static_assert(std::is_same_v<BanjoT, std::false_type> || std::is_same_v<BanjoT, ValueT>,
                "If the Banjo value type does not match the underlying integer type, use custom"
                "traits, override FromBanjo(), and document the safety of a static_cast");

  return banjo_value;
}

template <typename ValueT, typename FidlT, typename BanjoT>
constexpr FidlT DefaultIdTypeTraits<ValueT, FidlT, BanjoT>::ToFidl(const ValueT& value) noexcept {
  static_assert(std::is_same_v<decltype(FidlT::value), ValueT>,
                "If the FIDL struct's value type does not match the underlying integer type, use "
                "custom traits, override ToFidl(), and document the safety of a static_cast");

  return FidlT{.value = value};
}

template <typename ValueT, typename FidlT, typename BanjoT>
template <typename _>
constexpr BanjoT DefaultIdTypeTraits<ValueT, FidlT, BanjoT>::ToBanjo(const ValueT& value) noexcept {
  static_assert(std::is_same_v<BanjoT, std::false_type> || std::is_same_v<BanjoT, ValueT>,
                "If the Banjo value type does not match the underlying integer type, use custom"
                "traits, override ToBanjo(), and document the safety of a static_cast");

  return value;
}

}  // namespace display::internal

namespace std {

template <typename IdTraits>
struct hash<display::internal::IdType<IdTraits>> {
  size_t operator()(const display::internal::IdType<IdTraits>& id) const noexcept {
    return std::hash<typename IdTraits::ValueType>()(id.value());
  }
};

}  // namespace std

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ID_TYPE_H_

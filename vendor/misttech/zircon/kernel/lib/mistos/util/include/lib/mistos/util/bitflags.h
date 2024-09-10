// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BITFLAGS_V2_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BITFLAGS_V2_H_

#include <type_traits>

#include <ktl/optional.h>

template <typename Enum>
struct Flag {
  using Bits = std::underlying_type_t<Enum>;

  constexpr Flag(Bits& value) : value_(value) {}
  constexpr Flag(Enum value) : value_(static_cast<Bits>(value)) {}

  const Bits bits() const { return value_; }

 private:
  Bits value_;
};

template <typename Enum>
struct Flags {
  static constexpr size_t EMPTY = 0;
  static constexpr size_t ALL = ~0;

  using EnumType = Enum;
  using Bits = std::underlying_type_t<Enum>;

  static constexpr Flag<Enum> FLAGS[] = {};

  Flags() = delete;
  explicit Flags(Enum value) : bits_(static_cast<Bits>(value)) {
    static_assert((sizeof(FLAGS) / sizeof(FLAGS[0])) > 0);
  }

  static Flags empty() { return from_bits_retain(EMPTY); }

  static Flags all() {
    Bits truncated = EMPTY;
    for (const auto& flag : FLAGS) {
      truncated |= flag.bits();
    }
    return from_bits_retain(truncated);
  }

  inline Bits bits() const { return bits_; }
  Enum to_enum() const { return static_cast<Enum>(bits_); }

  static ktl::optional<Flags> from_bits(Bits bits) {
    auto truncated = from_bits_truncate(bits);
    return truncated.bits() == bits ? ktl::make_optional(truncated) : ktl::nullopt;
  }

  static Flags from_bits_truncate(Bits bits) { return from_bits_retain(bits & all().bits()); }

  static Flags from_bits_retain(Bits bits) { return Flags(bits); }

  // Whether all bits in this flags value are unset.
  bool is_empty() const { return bits() == EMPTY; }

  // Whether all known bits in this flags value are set.
  bool is_all() const { return (all().bits() | bits()) == bits(); }

  // Whether any set bits in a source flags value are also set in a target flags value.
  bool intersects(const Flags& other) const { return (bits() & other.bits()) != EMPTY; }

  // Whether all set bits in a source flags value are also set in a target flags value.
  bool contains(const Flags& other) const { return (bits() & other.bits()) == other.bits(); }
  bool contains(const Enum& value) const { return contains(Flags(value)); }

  // The bitwise or (`|`) of the bits in two flags values.
  void insert(const Flags& other) { *this = from_bits_retain(bits()).union_(other); }
  void insert(const Enum& value) { *this = from_bits_retain(bits()).union_(value); }

  // The intersection of a source flags value with the complement of a target flags value (`&!`).
  //
  // This method is not equivalent to `self & !other` when `other` has unknown bits set.
  // `remove` won't truncate `other`, but the `!` operator will.
  void remove(const Flags& other) { *this = from_bits_retain(bits()).difference(other); }
  void remove(const Enum& value) { *this = from_bits_retain(bits()).difference(value); }

  // The bitwise exclusive-or (`^`) of the bits in two flags values.
  // void toggle(const Flags& other) { *this = from_bits_retain(bits_ ^ other.bits_); }

  // Call [`Flags::insert`] when `value` is `true` or [`Flags::remove`] when `value` is `false`.
  /*void set(const Flags& other, bool value) {
    if (value) {
      insert(other);
    } else {
      remove(other);
    }
  }*/

  // The bitwise and (`&`) of the bits in two flags values.
  [[nodiscard]] Flags intersection(const Flags& other) const {
    return from_bits_retain(bits() & other.bits_);
  }
  [[nodiscard]] Flags intersection(const Enum& value) const { return intersection(Flags(value)); }

  // The bitwise or (`|`) of the bits in two flags values.
  [[nodiscard]] Flags union_(const Flags& other) const {
    return from_bits_retain(bits() | other.bits_);
  }
  [[nodiscard]] Flags union_(const Enum& value) const { return union_(Flags(value)); }

  // The intersection of a source flags value with the complement of a target flags value (`&!`).
  //
  // This method is not equivalent to `self & !other` when `other` has unknown bits set.
  // `difference` won't truncate `other`, but the `!` operator will.
  [[nodiscard]] Flags difference(const Flags& other) const {
    return from_bits_retain(bits() & ~other.bits_);
  }
  [[nodiscard]] Flags difference(const Enum& value) const { return difference(Flags(value)); }

  /// The bitwise exclusive-or (`^`) of the bits in two flags values.
  [[nodiscard]] Flags symmetric_difference(const Flags& other) const {
    return from_bits_retain(bits() ^ other.bits_);
  }

  /// The bitwise negation (`~`) of the bits in a flags value, truncating the result.
  [[nodiscard]] Flags complement() const { return from_bits_truncate(~bits()); }

  // C++ operators ----------

  bool operator==(const Flags& other) const { return bits_ == other.bits_; }
  bool operator!=(const Flags& other) const { return bits_ != other.bits_; }

  Flags operator|(const Flags& other) const { return union_(other); }
  Flags operator|(const Enum& value) const { return union_(value); }

  Flags& operator|=(const Flags& other) {
    insert(other);
    return *this;
  }
  Flags& operator|=(const Enum& value) {
    insert(value);
    return *this;
  }

  Flags operator&(const Flags& other) const { return intersection(other); }
  Flags operator&(const Enum& value) const { return intersection(value); }

  Flags& operator&=(const Flags& other) {
    *this = intersection(other);
    return *this;
  }
  Flags& operator&=(const Enum& value) {
    *this = intersection(value);
    return *this;
  }

  Flags operator-(const Flags& other) const { return difference(other); }
  Flags operator-(const Enum& value) const { return difference(value); }
  Flags& operator-=(const Flags& other) {
    *this = difference(other);
    return *this;
  }
  Flags& operator-=(const Enum& value) {
    *this = difference(value);
    return *this;
  }

  Flags operator!() const { return complement(); }

 private:
  explicit Flags(Bits value) : bits_(value) {}

  Bits bits_;
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BITFLAGS_V2_H_

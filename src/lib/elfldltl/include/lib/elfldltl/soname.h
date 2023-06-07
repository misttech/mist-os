// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SONAME_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SONAME_H_

#include <cassert>
#include <cstdint>
#include <string_view>

#include "abi-ptr.h"
#include "abi-span.h"
#include "gnu-hash.h"

namespace elfldltl {

// This provides an optimized type for holding a DT_SONAME / DT_NEEDED string.
// It always hashes the string to make equality comparisons faster.
template <class Elf = Elf<>, class AbiTraits = LocalAbiTraits>
class Soname {
 public:
  constexpr Soname() = default;

  constexpr Soname(const Soname&) = default;

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>,
            typename = std::enable_if_t<std::is_constructible_v<Ptr, const char*>>>
  constexpr explicit Soname(std::string_view name)
      : name_(name.data()), size_(static_cast<uint32_t>(name.size())), hash_(GnuHashString(name)) {
    assert(size_ == name.size());
  }

  constexpr Soname& operator=(const Soname&) noexcept = default;

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr std::string_view str() const {
    return {name_.get(), size_};
  }

  constexpr uint32_t hash() const { return hash_; }

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr bool operator==(const Soname& other) const {
    return other.hash_ == hash_ && other.str() == str();
  }

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr bool operator!=(const Soname& other) const {
    return other.hash_ != hash_ || other.str() != str();
  }

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr bool operator<(const Soname& other) const {
    return str() < other.str();
  }

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr bool operator<=(const Soname& other) const {
    return str() <= other.str();
  }

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr bool operator>(const Soname& other) const {
    return str() > other.str();
  }

  template <typename Ptr = AbiPtr<const char, Elf, AbiTraits>, typename = decltype(Ptr{}.get())>
  constexpr bool operator>=(const Soname& other) const {
    return str() >= other.str();
  }

 private:
  // This stores a pointer and 32-bit length directly rather than just using
  // std::string_view so that the whole object is still only two 64-bit words.
  // Crucially, both x86-64 and AArch64 ABIs pass and return trivial two-word
  // objects in registers but anything larger in memory, so this keeps passing
  // Soname as cheap as passing std::string_view.  This limits lengths to 4GiB,
  // which is far more than the practical limit.
  AbiPtr<const char, Elf, AbiTraits> name_;
  typename Elf::Word size_ = 0;
  typename Elf::Word hash_ = 0;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SONAME_H_

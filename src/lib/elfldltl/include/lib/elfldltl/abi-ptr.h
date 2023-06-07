// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_ABI_PTR_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_ABI_PTR_H_

#include <lib/stdcompat/version.h>

#include <cassert>
#include <cstdint>

#if __cpp_impl_three_way_comparison >= 201907L
#include <compare>
#endif

#include "layout.h"

namespace elfldltl {

// See below.
struct LocalAbiTraits;

// elfldltl::AbiPtr<T> is like a T* but can support a stable and cross-friendly
// ABI.  It can be used to store pointers that use a different byte order,
// address size, or address space, than the current process.  Its main purpose
// is for conversions from pointers in the local address space into pointers in
// a different address space and possibly different format.  The conversions
// are handled elsewhere (TODO(mcgrathr): not yet implemented).
//
// The byte order and address are represented by an elfldltl::Elf<...> type in
// the Elf template parameter; it defaults to the native elfldltl::Elf<>.  The
// type in the Traits template parameter determines whether these are actually
// used in how the pointer is stored; it defaults to LocalAbiTraits, which just
// stores a normal native pointer regardless of the Elf size and encoding.
//
// elfldltl::AbiPtr<T> should be used instead of normal T* in all data
// structures that will exist in one process address space but be populated by
// code running in a different address space.  Such data structures should be
// templated on Elf and AbiTraits classes that are propagated to the
// elfldltl::AbiPtr<T, ...> instantiations they use for pointer-type members.
//
// elfldltl::AbiPtr<T> always supports all the arithmetic operators that real
// T* pointers do.  The Traits type might or might not allow converting the
// stored pointer to a real pointer in the local address space.  If it does,
// then elfldltl::AbiPtr<T> can be used like a smart-pointer type with `get()`
// and `*` and `->` operators and can be constructed from real T* pointers.
//
// The Traits template API is described below.  The default LocalAbiTraits type
// just uses a normal pointer and supports all the the smart-pointer methods
// and operators.
template <typename T, class Elf = elfldltl::Elf<>, class Traits = LocalAbiTraits>
struct AbiPtr {
 public:
  using value_type = T;

  // This is the type used to represent an address in the target address space.
  using Addr = typename Elf::Addr;

  // No matter how the pointer is represented, sizes of objects or arrays it
  // points to must fit into size_type.
  using size_type = typename Elf::size_type;

  // The Traits type determines the underlying type stored.
  using StorageType = typename Traits::template StorageType<Elf, T>;

  constexpr AbiPtr() = default;

  constexpr AbiPtr(const AbiPtr&) = default;

  // If Traits::Get<T> is supported, then Traits::Make<T> is too.
  // In this case, the AbiPtr can be constructed from a normal pointer.
  template <class TT = Traits,
            typename = decltype(TT::template Get<value_type>(std::declval<StorageType>()))>
  constexpr explicit AbiPtr(value_type* ptr) : storage_(Traits::template Make<value_type>(ptr)) {}

  constexpr AbiPtr& operator=(const AbiPtr&) = default;

  static constexpr AbiPtr FromAddress(Addr address) {
    return AbiPtr{Traits::template FromAddress<Elf, T>(address), std::in_place};
  }

  constexpr explicit operator bool() const { return *this != AbiPtr{}; }

  constexpr bool operator==(const AbiPtr& other) const {
    return ComparisonValue() == other.ComparisonValue();
  }

  constexpr bool operator!=(const AbiPtr& other) const {
    return ComparisonValue() != other.ComparisonValue();
  }

#if __cpp_impl_three_way_comparison >= 201907L

  constexpr std::strong_ordering operator<=>(const AbiPtr& other) const {
    return ComparisonValue() <=> other.ComparisonValue();
  }

#else  // No operator<=> support.

  constexpr bool operator<(const AbiPtr& other) const {
    return ComparisonValue() < other.ComparisonValue();
  }

  constexpr bool operator>(const AbiPtr& other) const {
    return ComparisonValue() > other.ComparisonValue();
  }

  constexpr bool operator<=(const AbiPtr& other) const {
    return ComparisonValue() <= other.ComparisonValue();
  }

  constexpr bool operator>=(const AbiPtr& other) const {
    return ComparisonValue() >= other.ComparisonValue();
  }

#endif  // operator<=> support.

  constexpr AbiPtr operator+(size_type n) const {
    return AbiPtr{storage_ + (n * kScale), std::in_place};
  }

  constexpr AbiPtr operator-(size_type n) const {
    return AbiPtr{storage_ - (n * kScale), std::in_place};
  }

  constexpr size_type operator-(const AbiPtr& other) const {
    return static_cast<size_type>(storage_ - other.storage_) / kScale;
  }

  constexpr AbiPtr& operator+=(size_type n) {
    *this = *this + n;
    return *this;
  }

  constexpr AbiPtr& operator-=(size_type n) {
    *this = *this - n;
    return *this;
  }

  // This just returns the address in the target address space.
  constexpr size_type address() const {
    return static_cast<size_type>(Traits::GetAddress(storage_));
  }

  // Dereferencing methods are only available if enabled by the Traits type.

  template <typename TT = Traits,
            typename = decltype(TT::template Get<T>(std::declval<StorageType>()))>
  constexpr T* get() const {
    return Traits::template Get<T>(storage_);
  }

  template <typename TT = Traits,
            typename = decltype(TT::template Get<T>(std::declval<StorageType>()))>
  constexpr T* operator->() const {
    return get();
  }

  template <typename TT = Traits,
            typename = decltype(TT::template Get<T>(std::declval<StorageType>()))>
  constexpr T& operator*() const {
    return *get();
  }

 private:
  static constexpr size_type kScale = Traits::template kScale<T>;

  constexpr explicit AbiPtr(StorageType storage, std::in_place_t in_place) : storage_{storage} {}

  constexpr auto ComparisonValue() const { return Traits::ComparisonValue(storage_); }

  constexpr size_type Scale(size_type n) { return n * kScale; }

  StorageType storage_{};
};

// This is the default Traits type for AbiPtr and things that use it.
// It also serves as the exemplar for the template API.  Comments here
// describe what any Traits type must define.
struct LocalAbiTraits {
  // This must be defined to some type that admits + and - operators with
  // integer types, and operator- with itself.
  template <class Elf, typename T>
  using StorageType = T*;

  // This must be defined to a factor by which an integer should be scaled up
  // before adding or subtracting to StorageType<..., T>, and by which the
  // value of subtracting two StorageType<..., T> values should be scaled down.
  template <typename T>
  static constexpr uint32_t kScale = 1;

  // This must be defined to accept any StorageType<...> argument and yield the
  // address in the target address space as some unsigned integer type.
  static uintptr_t GetAddress(const void* ptr) { return reinterpret_cast<uintptr_t>(ptr); }

  // This must be defined to accept any StorageType<...> argument and yield an
  // integer type that admits operator<=> for comparison of two pointers it's
  // valid to compare by C rules.
  template <typename T>
  static constexpr auto ComparisonValue(T* ptr) {
    return ptr;
  }

  // This must be defined to accept Elf::Addr (or Elf::size_type, since they
  // are mutually convertible).
  template <class Elf, typename T>
  static StorageType<Elf, T> FromAddress(typename Elf::size_type address) {
    return reinterpret_cast<T*>(static_cast<uintptr_t>(address));
  }

  // The last two functions don't have to be defined, but if one is defined
  // then both must be defined.  They convert between a local T* pointer and
  // StorageType<Elf,T> and are only defined when such a conversion exists.

  template <class Elf, typename T>
  static constexpr StorageType<Elf, T> Make(T* ptr) {
    return ptr;
  }

  template <class Elf, typename T>
  static constexpr T* Get(StorageType<Elf, T> ptr) {
    return ptr;
  }
};

// This is a baseline Traits type for storing pointers in a different address
// space and pointer encoding.  An AbiPtr<..., RemoteAbiTraits> type doesn't
// support the get() method and the pointer-like operators.
//
// elfldltl::AbiPtr<T, Elf, elfldltl::RemoteAbiTraits> has in every context the
// same layout that elfldltl::AbiPtr<T> does wherever elfldltl::Elf<> is Elf.
struct RemoteAbiTraits {
  template <class Elf, typename T>
  using StorageType = typename Elf::Addr;

  template <typename T>
  static constexpr uint32_t kScale = sizeof(T);

  template <typename Addr>
  static constexpr auto GetAddress(Addr ptr) {
    return ptr();
  }

  template <typename Addr>
  static constexpr auto ComparisonValue(Addr ptr) {
    return ptr();
  }

  // This must be defined to accept Elf::Addr (or Elf::size_type, since they
  // are mutually convertible).
  template <class Elf, typename T>
  static constexpr StorageType<Elf, T> FromAddress(typename Elf::Addr address) {
    return address;
  }
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_ABI_PTR_H_

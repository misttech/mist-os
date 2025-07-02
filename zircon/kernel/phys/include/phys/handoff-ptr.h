// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_PTR_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_PTR_H_

// Note: we refrain from using the ktl namespace as <phys/handoff.h> is
// expected to be compiled in the userboot toolchain.

#include <stddef.h>
#include <zircon/assert.h>

#include <span>
#include <string_view>
#include <utility>

struct PhysHandoff;
extern PhysHandoff* gPhysHandoff;

// PhysHandoffPtr provides a "smart pointer" style API for pointers handed off
// from physboot to the kernel proper.  A handoff pointer is only ever created
// in physboot by the HandoffPrep class.  It's only ever dereferenced (or
// converted into a raw pointer) in the kernel proper.
//
// Lifetime issues for handoff data are complex.  PhysHandoffPtr is always
// treated as a traditional "owning" smart pointer and is a move-only type.
// Ordinarily, handoff pointer objects will be left in place and only have raw
// pointers extracted from them for later use.

// PhysHandoffPtr has no destructor and the "owning" pointer dying doesn't have
// any direct effect.  The lifetime of all handoff pointers is actually grouped
// in two buckets:
//
//  * Permanent handoff data will be accessible in the kernel's virtual address
//    space permanently.  This data resides on pages that the PMM has been told
//    are owned by kernel mappings.
//
//  * Temporary handoff data must be consumed only during the handoff phase,
//    which ends once EndHandoff() is called.  This data resides on pages that
//    the PMM may be told to reuse after handoff.
enum class PhysHandoffPtrLifetime { Permanent, Temporary };

#ifndef HANDOFF_PTR_DEREF
#ifdef _KERNEL
#define HANDOFF_PTR_DEREF 1
#else
#define HANDOFF_PTR_DEREF 0
#endif
#endif

template <typename T, PhysHandoffPtrLifetime Lifetime>
class PhysHandoffPtr {
 public:
  constexpr PhysHandoffPtr() = default;

  constexpr PhysHandoffPtr(const PhysHandoffPtr&) = delete;

  constexpr PhysHandoffPtr(PhysHandoffPtr&& other) noexcept : ptr_(std::exchange(other.ptr_, {})) {}

  constexpr PhysHandoffPtr& operator=(PhysHandoffPtr&& other) noexcept {
    ptr_ = std::exchange(other.ptr_, {});
    return *this;
  }

  constexpr auto operator<=>(const PhysHandoffPtr& other) const = default;

#if HANDOFF_PTR_DEREF
  // Handoff pointers can only be dereferenced in the kernel proper.

  T* get() const {
    if constexpr (Lifetime == PhysHandoffPtrLifetime::Temporary) {
      ZX_DEBUG_ASSERT_MSG(gPhysHandoff,
                          "Pointer no longer valid; phys hand-off has already ended!");
    }
    return ptr_;
  }

  T* release() { return std::exchange(ptr_, {}); }

  T& operator*() const { return *get(); }

  T* operator->() const { return get(); }
#endif  // HANDOFF_PTR_DEREF

 private:
  friend class HandoffPrep;

  T* ptr_ = nullptr;
};

// PhysHandoffSpan<T> is to std::span<T> as PhysHandoffPtr<T> is to T*.
// It has get() and release() methods that return std::span<T>.

template <typename T, PhysHandoffPtrLifetime Lifetime>
class PhysHandoffSpan {
 public:
  PhysHandoffSpan() = default;
  PhysHandoffSpan(const PhysHandoffSpan&) = delete;
  PhysHandoffSpan(PhysHandoffSpan&&) noexcept = default;

  PhysHandoffSpan(PhysHandoffPtr<T, Lifetime> ptr, size_t size) : ptr_(ptr), size_(size) {}

  PhysHandoffSpan& operator=(PhysHandoffSpan&&) noexcept = default;

  constexpr auto operator<=>(const PhysHandoffSpan& other) const = default;

  size_t size() const { return size_; }

  bool empty() const { return size() == 0; }

#if HANDOFF_PTR_DEREF
  std::span<T> get() const { return {ptr_.get(), size_}; }

  std::span<T> release() { return {ptr_.release(), size_}; }
#endif

 private:
  friend class HandoffPrep;

  PhysHandoffPtr<T, Lifetime> ptr_;
  size_t size_ = 0;
};

// PhysHandoffString is stored just the same as PhysHandoffSpan<const char>,
// but its get() and release() methods yield std::string_view.
template <PhysHandoffPtrLifetime Lifetime>
class PhysHandoffString : public PhysHandoffSpan<const char, Lifetime> {
 public:
  using Base = PhysHandoffSpan<const char, Lifetime>;

  PhysHandoffString() = default;
  PhysHandoffString(const PhysHandoffString&) = default;

#ifdef HANDOFF_PTR_DEREF
  std::string_view get() const {
    std::span str = Base::get();
    return {str.data(), str.size()};
  }

  std::string_view release() {
    std::span str = Base::release();
    return {str.data(), str.size()};
  }
#endif
};

// Convenience aliases used in the PhysHandoff declaration.

template <typename T>
using PhysHandoffTemporaryPtr = PhysHandoffPtr<T, PhysHandoffPtrLifetime::Temporary>;

template <typename T>
using PhysHandoffTemporarySpan = PhysHandoffSpan<T, PhysHandoffPtrLifetime::Temporary>;

using PhysHandoffTemporaryString = PhysHandoffString<PhysHandoffPtrLifetime::Temporary>;

template <typename T>
using PhysHandoffPermanentPtr = PhysHandoffPtr<T, PhysHandoffPtrLifetime::Permanent>;

template <typename T>
using PhysHandoffPermanentSpan = PhysHandoffSpan<T, PhysHandoffPtrLifetime::Permanent>;

using PhysHandoffPermanentString = PhysHandoffString<PhysHandoffPtrLifetime::Permanent>;

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_HANDOFF_PTR_H_

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_ABI_SPAN_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_ABI_SPAN_H_

#include "internal/abi-span.h"

namespace elfldltl {

// elfldltl::AbiSpan<T> is like a C++20 std::span<T> but with the same
// properties as elfldltl::AbiPtr<T> (see <lib/elfldltl/abi-ptr.h>): it can be
// used to store pointers and lengths that use a different byte order, address
// size, or address space, than the current process.
//
// It always supports the std::span constructors and operations that don't use
// take T* pointers or dereference into the span (size, subspan, etc.), but
// not the iterator methods (begin, end, etc.).
//
// When instantiated for the native byte order, address size, and address
// space, it supports all the other std::span<T> methods (though not all the
// fancy constructors), can be copy-constructed from std::span<T>, and also has
// an explicit `get()` method to return it.  The iterator methods (begin, end,
// etc.) return the std::span<T> iterator types directly.
template <typename T, size_t N = cpp20::dynamic_extent, class Elf = elfldltl::Elf<>,
          class Traits = LocalAbiTraits>
class AbiSpan : public internal::AbiSpanImpl<T, N, Elf, Traits> {
 public:
  using typename internal::AbiSpanImpl<T, N, Elf, Traits>::Ptr;
  using typename internal::AbiSpanImpl<T, N, Elf, Traits>::size_type;

  constexpr AbiSpan() = default;

  constexpr AbiSpan(const AbiSpan&) = default;

  constexpr AbiSpan(Ptr ptr, size_type size) noexcept
      : internal::AbiSpanImpl<T, N, Elf, Traits>{ptr} {
    assert(size == N);
  }

  // Allow copy-construction from the corresponding span type if it admits
  // construction from a pointer.  Note that span's "range" copy-constructor
  // already allows the other direction because AbiSpan has data() and size().
  template <typename P = T*, typename = std::enable_if_t<std::is_constructible_v<Ptr, P>>>
  constexpr AbiSpan(cpp20::span<T, N> other)
      : internal::AbiSpanImpl<T, N, Elf, Traits>{Ptr{other.data()}} {}

  constexpr size_type size() const { return static_cast<size_type>(N); }
};

template <typename T, class Elf, class Traits>
class AbiSpan<T, cpp20::dynamic_extent, Elf, Traits>
    : public internal::AbiSpanImpl<T, cpp20::dynamic_extent, Elf, Traits> {
 public:
  using typename internal::AbiSpanImpl<T, cpp20::dynamic_extent, Elf, Traits>::Ptr;
  using typename internal::AbiSpanImpl<T, cpp20::dynamic_extent, Elf, Traits>::Addr;
  using typename internal::AbiSpanImpl<T, cpp20::dynamic_extent, Elf, Traits>::size_type;

  constexpr AbiSpan() = default;

  constexpr AbiSpan(const AbiSpan&) = default;

  constexpr AbiSpan(Ptr ptr, size_type size) noexcept
      : internal::AbiSpanImpl<T, cpp20::dynamic_extent, Elf, Traits>{ptr}, size_(size) {}

  // See comment above.
  template <typename TT = T, typename = std::enable_if_t<std::is_constructible_v<Ptr, TT*>>>
  constexpr AbiSpan(cpp20::span<T> other)
      : internal::AbiSpanImpl<T, cpp20::dynamic_extent, Elf, Traits>{Ptr{other.data()}},
        size_{static_cast<size_type>(other.size())} {}

  constexpr AbiSpan& operator=(const AbiSpan&) = default;

  constexpr size_type size() const { return size_; }

 private:
  Addr size_ = 0;
};

// elfldltl::AbiStringView stands in for std::string_view, but is actually
// based on elfldltl::AbiSpan<const char>.
//
// It only replicates the most basic methods of std::string_view.  When
// dereferencing is supported by the Traits type, it just provides a `get()`
// method and implicit conversion to and from std::string_view so it can be
// copied into a std::string_view to call the various methods.
template <class Elf = elfldltl::Elf<>, class Traits = LocalAbiTraits>
class AbiStringView {
 public:
  using Span = AbiSpan<const char, cpp20::dynamic_extent, Elf, Traits>;
  using Ptr = typename Span::Ptr;
  using size_type = typename Span::size_type;

  constexpr AbiStringView() = default;

  constexpr AbiStringView(const AbiStringView&) = default;

  constexpr explicit AbiStringView(Span contents) : contents_(contents) {}

  constexpr AbiStringView(Ptr ptr, size_type length) : contents_{ptr, length} {}

  template <typename S = Span,
            typename = std::enable_if_t<std::is_constructible_v<S, cpp20::span<const char>>>>
  constexpr AbiStringView(std::string_view str) : contents_{cpp20::span{str}} {}

  constexpr AbiStringView& operator=(const AbiStringView&) = default;

  constexpr bool empty() const { return contents_.empty(); }

  constexpr size_t size() const { return contents_.size(); }
  constexpr size_t length() const { return contents_.size(); }

  template <typename S = Span, typename = decltype(S{}.get())>
  constexpr operator std::string_view() const {
    return get();
  }

  template <typename S = Span, typename = decltype(S{}.data())>
  constexpr std::string_view get() const {
    return {data(), size()};
  }

  template <typename S = Span, typename = decltype(S{}.data())>
  constexpr const char* data() const {
    return contents_.data();
  }

 private:
  Span contents_;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_ABI_SPAN_H_

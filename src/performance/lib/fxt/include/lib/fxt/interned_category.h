// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_INTERNED_CATEGORY_H_
#define SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_INTERNED_CATEGORY_H_

#include <lib/fxt/interned_string.h>
#include <lib/fxt/section_symbols.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <atomic>

namespace fxt {

// Represents an internalized tracing category and its associated bit in the enabled categories
// bitmap. This type does not define constructors or a destructor and contains trivially
// constructible members so that it may be aggregate-initialized to avoid static initializers and
// guards.
//
// This type uses the same linker section collection method as InternedString. See
// lib/fxt/interned_string.h for more details.
//
struct InternedCategory {
  static constexpr uint32_t kInvalidBitNumber = -1;

  const InternedString& label;
  mutable std::atomic<uint32_t> bit_number{kInvalidBitNumber};
  mutable InternedCategory* next{nullptr};

  const char* string() const { return label.string; }

  uint32_t GetBit() const {
    const uint32_t bit = bit_number.load(std::memory_order_relaxed);
    return bit == kInvalidBitNumber ? Register(*this) : bit;
  }

  // Returns the head of the global category linked list.
  static const InternedCategory* head() { return head_.load(std::memory_order_acquire); }

  // Returns the begin and end pointers of the interned category section.
  static constexpr const InternedCategory* section_begin() {
    return __start___fxt_interned_category_table;
  }
  static constexpr const InternedCategory* section_end() {
    return __stop___fxt_interned_category_table;
  }

  // Forward iterator over the set of registered categories using the linked list.
  class ListIterator {
   public:
    ListIterator() = default;
    ListIterator(const ListIterator&) = default;
    ListIterator& operator=(const ListIterator&) = default;

    const InternedCategory& operator*() { return *node_; }

    ListIterator operator++() {
      node_ = node_->next;
      return *this;
    }

    bool operator!=(const ListIterator& other) { return node_ != other.node_; }

    struct Iterable {
      ListIterator begin() const { return ListIterator{head()}; }
      ListIterator end() const { return ListIterator{nullptr}; }
    };

   private:
    explicit ListIterator(const InternedCategory* node) : node_{node} {}

    const InternedCategory* node_{nullptr};
  };

  // Forward iterator over the set of categories collected using the compile-time linker section.
  class SectionIterator {
   public:
    SectionIterator() = default;
    SectionIterator(const SectionIterator&) = default;
    SectionIterator& operator=(const SectionIterator&) = default;

    const InternedCategory& operator*() { return *node_; }

    SectionIterator operator++() {
      node_++;
      return *this;
    }

    bool operator!=(const SectionIterator& other) { return node_ != other.node_; }

    struct Iterable {
      SectionIterator begin() const { return SectionIterator{section_begin()}; }
      SectionIterator end() const { return SectionIterator{section_end()}; }
    };

   private:
    explicit SectionIterator(const InternedCategory* node) : node_{node} {}
    const InternedCategory* node_{nullptr};
  };

  // Utility constexpr instances of the respective iterables.
  static constexpr ListIterator::Iterable IterateList{};
  static constexpr SectionIterator::Iterable IterateSection{};

  static void PreRegister() {
    for (const InternedCategory& interned_category : IterateSection) {
      Register(interned_category);
    }
  }

  using RegisterCategoryCallback = uint32_t (*)(const InternedCategory& category);

  // Sets the callback to register categories in the host trace environment.
  static void SetRegisterCategoryCallback(RegisterCategoryCallback callback) {
    register_category_callback_ = callback;
  }

  // Register an already initialized category.
  static void RegisterInitialized(const InternedCategory& interned_category) {
    ZX_DEBUG_ASSERT(interned_category.next == nullptr);
    interned_category.next = head_.load(std::memory_order_relaxed);
    while (!head_.compare_exchange_weak(interned_category.next,
                                        const_cast<InternedCategory*>(&interned_category),
                                        std::memory_order_release, std::memory_order_relaxed)) {
    }
  }

 private:
  [[gnu::noinline]] static uint32_t Register(const InternedCategory& interned_category) {
    uint32_t bit_number = interned_category.bit_number.load(std::memory_order_relaxed);
    if (bit_number != kInvalidBitNumber) {
      return bit_number;
    }

    if (register_category_callback_) {
      bit_number = register_category_callback_(interned_category);
      if (bit_number != kInvalidBitNumber) {
        RegisterInitialized(interned_category);
      }
    }

    return bit_number;
  }

  inline static std::atomic<InternedCategory*> head_{nullptr};
  inline static RegisterCategoryCallback register_category_callback_{nullptr};
};

namespace internal {

// Indirection to allow the literal operator below to be constexpr. Addresses of variables with
// static storage duration are valid constant expressions, however, constexpr functions may not
// directly declare local variables with static storage duration.
template <char... chars>
struct InternedCategoryStorage {
  inline static InternedCategory interned_category FXT_INTERNED_CATEGORY_SECTION{
      InternedStringStorage<chars...>::interned_string};

  // Since InternedCategory has mutable members it must not be placed in a read-only data section.
  static_assert(!std::is_const_v<decltype(interned_category)>);
};

}  // namespace internal

// String literal template operator that generates a unique InternedCategory instance for the given
// string literal. Every invocation for a given string literal value returns the same
// InternedCategory instance, such that the set of InternedCategory instances behaves as an
// internalized category table.
//
// This implementation uses the N3599 extension supported by Clang and GCC.  C++20 ratified a
// slightly different syntax that is simple to switch to, once available, without affecting call
// sites.
// TODO(https://fxbug.dev/42108463): Update to C++20 syntax when available.
//
// References:
//     http://open-std.org/JTC1/SC22/WG21/docs/papers/2013/n3599.html
//     http://www.open-std.org/jtc1/sc22/wg21/docs/papers/2017/p0424r2.pdf
//
// Example:
//     using fxt::operator""_category;
//     const fxt::InternedCategory& category = "FooBar"_category;
//
template <typename T, T... chars>
constexpr const InternedCategory& operator""_category() {
  static_assert(std::is_same_v<T, char>);
  return internal::InternedCategoryStorage<chars...>::interned_category;
}

}  // namespace fxt

#endif  // SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_INTERNED_CATEGORY_H_

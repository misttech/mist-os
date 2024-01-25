// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_INTERNED_STRING_H_
#define SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_INTERNED_STRING_H_

#include <lib/fxt/section_symbols.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <atomic>

namespace fxt {

// Represents an internalized string that may be referenced in traces by id to improve the
// efficiency of labels and other strings. This type does not define constructors or a destructor
// and contains trivially constructible members so that it may be aggregate-initialized to avoid
// static initializers and guards.
struct InternedString {
  static constexpr uint16_t kInvalidId = 0;
  static constexpr size_t kMaxStringLength = 32000;

  const char* string{nullptr};
  mutable std::atomic<uint16_t> id{kInvalidId};
  mutable InternedString* next{nullptr};

  // Returns the numeric id for this string ref. If this is the first runtime encounter with this
  // string ref a new id is generated and the string ref is added to the global linked list.
  uint16_t GetId() const {
    const uint16_t ref_id = id.load(std::memory_order_relaxed);
    return ref_id == kInvalidId ? Register(*this) : ref_id;
  }

  // Returns the head of the global string ref linked list.
  static const InternedString* head() { return head_.load(std::memory_order_acquire); }

  // Returns the begin and end pointers of the interned string section.
  static constexpr const InternedString* section_begin() {
    return __start___fxt_interned_string_table;
  }
  static constexpr const InternedString* section_end() {
    return __stop___fxt_interned_string_table;
  }

  // Forward iterator over the set of registered strings using the linked list.
  class ListIterator {
   public:
    ListIterator() = default;
    ListIterator(const ListIterator&) = default;
    ListIterator& operator=(const ListIterator&) = default;

    const InternedString& operator*() { return *node_; }

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
    explicit ListIterator(const InternedString* node) : node_{node} {}

    const InternedString* node_{nullptr};
  };

  // Forward iterator over the set of strings collected using the compile-time linker section.
  class SectionIterator {
   public:
    SectionIterator() = default;
    SectionIterator(const SectionIterator&) = default;
    SectionIterator& operator=(const SectionIterator&) = default;

    const InternedString& operator*() { return *node_; }

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
    explicit SectionIterator(const InternedString* node) : node_{node} {}
    const InternedString* node_{nullptr};
  };

  // Utility constexpr instances of the respective iterables.
  //
  // Examples:
  //
  // for (const auto&
  static constexpr ListIterator::Iterable IterateList{};
  static constexpr SectionIterator::Iterable IterateSection{};

  // Pre-registers all InternedString instances on supported compilers.
  //
  // Clang correctly implements section attributes on static template members in ELF targets,
  // resulting in every InternedString instance from instantiations of the _intern literal operator
  // being placed in the "__fxt_interned_string_table" section. However, GCC ignores section
  // attributes on COMDAT symbols as of this writing, resulting in an empty section when compiled
  // with GCC.  TODO(https://fxbug.dev/42101573): Cleanup this comment when GCC supports section attributes on
  // COMDAT.

  static void PreRegister() {
    for (const InternedString& interned_string : IterateSection) {
      Register(interned_string);
    }
  }

  using MapStringCallback = void (*)(uint16_t id, const char* string, size_t length);

  // Sets the callback to map strings in the host trace environment.
  static void SetMapStringCallback(MapStringCallback callback) { map_string_callback_ = callback; }

 private:
  // TODO(https://fxbug.dev/42108473): Replace runtime lock-free linked list with comdat linker sections once
  // the toolchain supports it with all compilers.
  [[gnu::noinline]] static uint16_t Register(const InternedString& interned_string) {
    // Return the id if the string ref is already registered.
    uint16_t id = interned_string.id.load(std::memory_order_relaxed);
    if (id != kInvalidId) {
      return id;
    }

    // Try to set the id of the string ref. When there is a race with other threads or CPUs only one
    // will succeed, in which case the id counter will harmlessly skip values equal to the number of
    // agents racing between here and the check above.
    const uint16_t new_id = id_counter_.fetch_add(1, std::memory_order_relaxed);
    while (!interned_string.id.compare_exchange_weak(id, new_id, std::memory_order_relaxed,
                                                     std::memory_order_relaxed)) {
      // If another agent set the id first simply return the id.
      if (id != kInvalidId && id != new_id)
        return id;
    }

    // Map the string value to the id the first time this interned string is encountered at runtime.
    ZX_DEBUG_ASSERT(id <= 0x7FFF);
    if (map_string_callback_) {
      map_string_callback_(id, interned_string.string,
                           strnlen(interned_string.string, kMaxStringLength));
    }

    // Register the string ref in the global linked list. When there is a race above only the
    // winning agent that set the id will continue to this point.
    interned_string.next = head_.load(std::memory_order_relaxed);
    while (!head_.compare_exchange_weak(interned_string.next,
                                        const_cast<InternedString*>(&interned_string),
                                        std::memory_order_release, std::memory_order_relaxed)) {
    }

    return new_id;
  }

  inline static std::atomic<uint16_t> id_counter_{kInvalidId + 1};
  inline static std::atomic<InternedString*> head_{nullptr};
  inline static MapStringCallback map_string_callback_{nullptr};
};

namespace internal {

// Indirection to allow the literal operator below to be constexpr. Addresses of variables with
// static storage druation are valid constant expressions, however, constexpr functions may not
// directly declare local variables with static storage duration.
template <char... chars>
struct InternedStringStorage {
  inline static const char storage[] = {chars..., '\0'};
  inline static InternedString interned_string FXT_INTERNED_STRING_SECTION{storage};

  // Since InternedString has mutable members it must not be placed in a read-only data section.
  static_assert(!std::is_const_v<decltype(interned_string)>);
};

}  // namespace internal

// String literal template operator that generates a unique InternedString instance for the given
// string literal. Every invocation for a given string literal value returns the same InternedString
// instance, such that the set of InternedString instances behaves as an internalized string table.
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
//     using fxt::operator""_intern;
//     const fxt::InternedString& string = "FooBar"_intern;
//

template <typename T, T... chars>
constexpr const InternedString& operator""_intern() {
  static_assert(std::is_same_v<T, char>);
  return internal::InternedStringStorage<chars...>::interned_string;
}

}  // namespace fxt

#endif  // SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_INTERNED_STRING_H_

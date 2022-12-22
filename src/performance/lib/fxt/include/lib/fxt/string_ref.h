// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Given a Writer implementing the Writer interface in writer-internal.h, provide an api
// over the writer to allow serializing fxt to the Writer.
//
// Based heavily on libTrace in zircon/system/ulib/trace to allow compatibility,
// but modified to enable passing in an arbitrary buffering system.

#ifndef SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_STRING_REF_H_
#define SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_STRING_REF_H_

#include <stdint.h>
#include <string.h>
#include <zircon/assert.h>

#include <type_traits>

#include "interned_string.h"
#include "record_types.h"

namespace fxt {

// Represents an FXT StringRecord which is either inline in the record body, or
// an index included in the record header.
//
// See also: https://fuchsia.dev/fuchsia-src/reference/tracing/trace-format#string-record
template <RefType>
class StringRef;

template <typename T, RefType ref_type>
using EnableIfConvertibleToStringRef =
    std::enable_if_t<(std::is_convertible_v<T, StringRef<ref_type>> &&
                      !std::is_same_v<T, StringRef<ref_type>>),
                     bool>;

template <>
class StringRef<RefType::kInline> {
  enum Convert { kConvert };

 public:
  static constexpr size_t kMaxStringLength = InternedString::kMaxStringLength;

  template <typename T, EnableIfConvertibleToStringRef<T, RefType::kInline> = true>
  StringRef(const T& value) : StringRef{kConvert, value} {}

  explicit StringRef(const char* string, size_t size = kMaxStringLength)
      : string_{string},
        size_{strnlen(string, size < kMaxStringLength ? size : kMaxStringLength)} {}

  StringRef(const StringRef&) = default;
  StringRef& operator=(const StringRef&) = default;

  WordSize PayloadSize() const { return WordSize::FromBytes(size_); }

  uint64_t HeaderEntry() const { return 0x8000 | size_; }

  template <typename Reservation>
  void Write(Reservation& res) const {
    res.WriteBytes(string_, size_);
  }

  size_t size() const { return size_; }

 private:
  StringRef(Convert, const StringRef& value) : string_{value.string_}, size_{value.size_} {}

  const char* string_;
  size_t size_;
};

#if __cplusplus >= 201703L
StringRef(const char*)->StringRef<RefType::kInline>;

StringRef(const char*, size_t)->StringRef<RefType::kInline>;

template <typename T, EnableIfConvertibleToStringRef<T, RefType::kInline> = true>
StringRef(const T&) -> StringRef<RefType::kInline>;
#endif

template <>
class StringRef<RefType::kId> {
  enum Convert { kConvert };

 public:
  template <typename T, EnableIfConvertibleToStringRef<T, RefType::kId> = true>
  StringRef(const T& value) : StringRef{kConvert, value} {}

  explicit StringRef(uint16_t id) : id_(id) {
    ZX_ASSERT_MSG(id < 0x8000, "The msb of a StringRef's id must be 0");
  }

  StringRef(const InternedString& interned_string) : StringRef{interned_string.GetId()} {}

  StringRef(const StringRef&) = default;
  StringRef& operator=(const StringRef&) = default;

  static WordSize PayloadSize() { return WordSize(0); }

  uint64_t HeaderEntry() const { return id_; }

  template <typename Reservation>
  void Write(Reservation& res) const {
    // Nothing, data in in the header
  }

 private:
  StringRef(Convert, const StringRef& value) : id_{value.id_} {}

  uint16_t id_;
};

#if __cplusplus >= 201703L
StringRef(uint16_t)->StringRef<RefType::kId>;

StringRef(const InternedString&)->StringRef<RefType::kId>;

template <typename T, EnableIfConvertibleToStringRef<T, RefType::kId> = true>
StringRef(const T&) -> StringRef<RefType::kId>;
#endif

}  // namespace fxt

#endif  // SRC_PERFORMANCE_LIB_FXT_INCLUDE_LIB_FXT_STRING_REF_H_

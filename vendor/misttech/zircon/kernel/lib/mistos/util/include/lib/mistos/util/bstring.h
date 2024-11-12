// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BSTRING_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BSTRING_H_

#include <lib/mistos/util/back_insert_iterator.h>
#include <stdio.h>
#include <zircon/assert.h>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/string_view.h>

class BString {
 public:
  // Creates an empty string.
  // Does not allocate heap memory.
  BString() { InitWithEmpty(); }

  // Creates a copy of another string.
  BString(const BString& other) : BString(ktl::string_view(other)) {}

  // Move constructs from another string.
  // The other string is set to empty.
  // Does not allocate heap memory.
  BString(BString&& other) {
    data_.swap(other.data_);
    other.data_.reset();
  }

  // Creates a string from the contents of a null-terminated C string.
  // Allocates heap memory only if |data| is non-empty.
  // |data| must not be null.
  BString(const char* data) { Init(data, ktl::string_view(data).size()); }

  // Creates a string from the contents of a character array of given length.
  // Allocates heap memory only if |length| is non-zero.
  // |data| must not be null.
  BString(const char* data, size_t length) { Init(data, length); }

  // Creates a string with |count| copies of |ch|.
  // Allocates heap memory only if |count| is non-zero.
  BString(size_t count, char ch) { Init(count, ch); }

  // Creates a string from the contents of a string.
  // Allocates heap memory only if |str.length()| is non-zero.
  BString(ktl::string_view str) : BString(str.data(), str.length()) {}

  // Returns a pointer to the null-terminated contents of the string.
  const char* data() const { return data_.data(); }
  const char* c_str() const { return data(); }

  // Returns the length of the string, excluding its null terminator.
  size_t length() const { return data_.size() - 1; }
  size_t size() const { return length(); }

  // Returns true if the string's length is zero.
  bool empty() const { return size() == 0U; }

  // Iterators
  const char* begin() const { return data(); }
  const char* cbegin() const { return data(); }
  const char* end() const { return data() + size(); }
  const char* cend() const { return data() + size(); }

  // Gets the character at the specified index.
  // Position must be greater than or equal to 0 and less than |length()|.
  const char& operator[](size_t pos) const { return data()[pos]; }

  // Performs a lexicographical character by character comparison.
  // Returns a negative value if |*this| comes before |other| in lexicographical order.
  // Returns zero if the strings are equivalent.
  // Returns a positive value if |*this| comes after |other| in lexicographical order.
  int compare(const BString& other) const;

  // Assigns this string to a copy of another string.
  BString& operator=(const BString& other) {
    data_.reset();
    Init(other.data(), other.length());
    return *this;
  }

  // Move assigns from another string.
  // The other string is set to empty.
  // Does not allocate heap memory.
  BString& operator=(BString&& other) noexcept {
    data_.reset();
    data_.swap(other.data_);
    return *this;
  }

  // Create a std::string_view backed by the string.
  // The view does not take ownership of the data so the string
  // must outlast the std::string_view.
  operator std::string_view() const { return {data(), length()}; }

 private:
  void Init(const char* data, size_t length);
  void Init(size_t count, char ch);
  void InitWithEmpty();

  fbl::Vector<char> data_;
};

bool operator==(const BString& lhs, const BString& rhs);

inline bool operator!=(const BString& lhs, const BString& rhs) { return !(lhs == rhs); }

inline bool operator<(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) < 0; }

inline bool operator>(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) > 0; }

inline bool operator<=(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) <= 0; }

inline bool operator>=(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) >= 0; }

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BSTRING_H_

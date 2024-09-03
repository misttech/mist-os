// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BSTRING_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BSTRING_H_

#include <lib/mistos/util/back_insert_iterator.h>
#include <stdio.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/string_view.h>

class BString {
 public:
  // Creates an empty string.
  // Does not allocate heap memory.
  BString() = default;

  // Creates a copy of another string.
  BString(const BString& other) {
    ktl::copy(other.begin(), other.end(), util::back_inserter(data_));
  }

  // Move constructs from another string.
  // The other string is set to empty.
  // Does not allocate heap memory.
  BString(BString&& other) {
    data_.swap(other.data_);
    other.data_.reset();
  }

  BString(const ktl::string_view& str) {
    ktl::copy(str.begin(), str.end(), util::back_inserter(data_));
  }

  BString(const char* data, size_t size) {
    ktl::copy(data, data + size, util::back_inserter(data_));
  }

  BString(const char* data) {
    const ktl::string_view tmp(data);
    ktl::copy(tmp.begin(), tmp.end(), util::back_inserter(data_));
  }

  // Returns a pointer to the null-terminated contents of the string.
  const char* data() const { return data_.data(); }
  const char* c_str() const { return data(); }

  size_t size() const { return data_.size(); }

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
    ktl::copy(other.begin(), other.end(), util::back_inserter(data_));
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
  operator ktl::string_view() const { return {data(), size()}; }

 private:
  fbl::Vector<char> data_;
};

bool operator==(const BString& lhs, const BString& rhs);

inline bool operator!=(const BString& lhs, const BString& rhs) { return !(lhs == rhs); }

inline bool operator<(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) < 0; }

inline bool operator>(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) > 0; }

inline bool operator<=(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) <= 0; }

inline bool operator>=(const BString& lhs, const BString& rhs) { return lhs.compare(rhs) >= 0; }

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_BSTRING_H_

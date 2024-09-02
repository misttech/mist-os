// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_

#include <lib/mistos/util/back_insert_iterator.h>
#include <stdio.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/string_view.h>

namespace starnix {

class BString {
 public:
  // Constructors
  BString() = default;

  // Creates a copy of another string.
  BString(const BString& other) {
    ktl::copy(other.begin(), other.end(), util::back_inserter(data_));
  }

  // Move constructs from another string.
  // The other string is set to empty.
  // Does not allocate heap memory.
  BString(BString&& other) { data_.swap(other.data_); }

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

  // Assigns this string to a copy of another string.
  BString& operator=(BString& other) {
    data_.reset();
    ktl::copy(other.begin(), other.end(), util::back_inserter(data_));
    return *this;
  }

  // Move assigns from another string.
  // The other string is set to empty.
  // Does not allocate heap memory.
  BString& operator=(BString&& other) {
    data_.reset();
    data_.swap(other.data_);
    return *this;
  }

  // Accessors
  const char* data() const { return data_.data(); }
  size_t size() const { return data_.size(); }

  // Iterators
  const char* begin() const { return data_.begin(); }
  const char* end() const { return data_.end(); }

 private:
  fbl::Vector<char> data_;
};

using FsString = BString;
using FsStr = ktl::string_view;

constexpr char SEPARATOR = '/';

// Helper that can be used to build paths backwards, from the tail to head.
class PathBuilder {
 public:
  void prepend_element(const FsStr& element);

  FsString build_absolute();

  FsString build_relative();

 private:
  static constexpr size_t INITIAL_CAPACITY = 32;

  void ensure_capacity(size_t capacity_needed);

  // The path is kept in `data[pos..]`.
  fbl::Vector<char> data_;

  size_t pos_ = 0;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_

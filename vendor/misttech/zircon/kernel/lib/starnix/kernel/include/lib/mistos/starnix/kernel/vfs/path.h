// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_

#include <stdio.h>

#include <algorithm>
#include <functional>
#include <vector>

#include <fbl/intrusive_single_list.h>
#include <fbl/string.h>

namespace starnix {

using FsString = fbl::String;
using FsStr = fbl::String;

constexpr char SEPARATOR = '/';

// Helper that can be used to build paths backwards, from the tail to head.
class PathBuilder {
 public:
  PathBuilder() : pos_(0) {}

  void prepend_element(const FsStr& element) {
    ensure_capacity(element.size() + 1);
    pos_ -= element.size() + 1;
    std::copy(element.begin(), element.end(), data_.begin() + pos_ + 1);
    data_[pos_] = '/';
  }

  FsString build_absolute() {
    if (pos_ == data_.size()) {
      return "/";
    }
    data_.erase(data_.begin(), data_.begin() + pos_);
    return FsString(data_.data(), data_.size());
  }

  FsString build_relative() {
    FsString absolute = build_absolute();
    std::vector<char> vec(absolute.begin(), absolute.end());
    vec.erase(vec.begin());
    return FsString(vec.data(), vec.size());
  }

 private:
  static constexpr size_t INITIAL_CAPACITY = 32;

  void ensure_capacity(size_t capacity_needed) {
    if (capacity_needed > pos_) {
      auto current_size = data_.size();
      auto len = current_size - pos_;
      auto min_size = len + capacity_needed;
      auto new_size = std::max(current_size * 2, INITIAL_CAPACITY);
      while (new_size < min_size) {
        new_size *= 2;
      }
      data_.reserve(new_size - current_size);
      data_.resize(new_size - len, 0);
      std::copy(data_.data() + pos_, data_.data() + pos_ + len, std::back_inserter(data_));
      pos_ = new_size - len;
    }
  }

  // The path is kept in `data[pos..]`.
  std::vector<char> data_;
  size_t pos_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_

// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/path.h"

#include <stdio.h>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/string_view.h>

#include <ktl/enforce.h>

namespace starnix {

// Helper that can be used to build paths backwards, from the tail to head.
void PathBuilder::prepend_element(const FsStr& element) {
  ensure_capacity(element.size() + 1);
  pos_ -= element.size() + 1;
  size_t idx = 0;
  ktl::for_each(element.begin(), element.end(),
                [&](char value) { data_[pos_ + 1 + idx++] = value; });
  data_[pos_] = '/';
}

FsString PathBuilder::build_absolute() {
  if (pos_ == data_.size()) {
    return "/";
  }
  for (size_t i = 0; i < pos_; ++i) {
    data_.erase(0);
  }
  return FsString(data_.data(), data_.size());
}

FsString PathBuilder::build_relative() {
  FsString absolute = build_absolute();
  fbl::Vector<char> vec;
  ktl::copy(absolute.begin(), absolute.end(), util::back_inserter(vec));
  vec.erase(0);
  return FsString(vec.data(), vec.size());
}

void PathBuilder::ensure_capacity(size_t capacity_needed) {
  if (capacity_needed > pos_) {
    auto current_size = data_.size();
    auto len = current_size - pos_;
    auto min_size = len + capacity_needed;
    auto new_size = ktl::max(current_size * 2, INITIAL_CAPACITY);
    while (new_size < min_size) {
      new_size *= 2;
    }
    fbl::AllocChecker ac;
    data_.reserve(new_size - current_size, &ac);
    ASSERT(ac.check());
    data_.resize(new_size - len, &ac);
    ASSERT(ac.check());
    ktl::copy(data_.data() + pos_, data_.data() + pos_ + len, util::back_inserter(data_));
    pos_ = new_size - len;
  }
}

}  // namespace starnix

// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/path.h"

#include <lib/mistos/util/back_insert_iterator.h>
#include <stdio.h>

#include <algorithm>
#include <ranges>

#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/string_view.h>

// #include <ktl/enforce.h>

namespace starnix {

namespace {

// To match Rust's Vec impl

template <typename T>
void reserve(fbl::Vector<T>& vec, size_t additional) {
  fbl::AllocChecker ac;
  vec.reserve(vec.size() + additional, &ac);
  ZX_ASSERT(ac.check());
}

template <typename T>
void resize(fbl::Vector<T>& vec, size_t new_size, const T& value) {
  fbl::AllocChecker ac;
  size_t old_size = vec.size();
  vec.resize(new_size, &ac);
  ZX_ASSERT(ac.check());
  if (new_size > old_size) {
    ktl::fill(vec.begin() + old_size, vec.end(), value);
  }
}

template <typename T>
void extend_from_within(fbl::Vector<T>& vec, typename fbl::Vector<T>::iterator start, size_t len) {
  auto range = std::ranges::subrange(start, start + len);
  reserve(vec, range.size());
  std::ranges::copy(range, util::back_inserter(vec));
}

}  // namespace

PathBuilder PathBuilder::New() { return PathBuilder(); }

// Helper that can be used to build paths backwards, from the tail to head.
void PathBuilder::prepend_element(const FsStr& element) {
  ensure_capacity(element.size() + 1);
  pos_ -= element.size() + 1;
  size_t idx = 0;
  std::ranges::for_each(element, [&](char value) { data_[pos_ + 1 + idx++] = value; });
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
  std::ranges::copy(absolute, util::back_inserter(vec));
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
    reserve(data_, new_size - current_size);
    resize<char>(data_, new_size - len, 0);
    extend_from_within(data_, data_.begin() + pos_, len);
    pos_ = new_size - len;
  }
}

}  // namespace starnix

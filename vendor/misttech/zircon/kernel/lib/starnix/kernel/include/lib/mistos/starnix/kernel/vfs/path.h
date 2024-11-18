// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_

#include <lib/mistos/util/bstring.h>
#include <stdio.h>

#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/string_view.h>

namespace starnix {

using FsString = mtl::BString;
using FsStr = ktl::string_view;

constexpr char SEPARATOR = '/';

// Helper that can be used to build paths backwards, from the tail to head.
class PathBuilder {
 private:
  // The path is kept in `data[pos..]`.
  fbl::Vector<char> data_;
  size_t pos_ = 0;

  // impl PathBuilder
  static constexpr size_t INITIAL_CAPACITY = 32;

 public:
  static PathBuilder New();

  void prepend_element(const FsStr& element);

  /// Build the absolute path string.
  FsString build_absolute();

  /// Build the relative path string.
  FsString build_relative();

 private:
  void ensure_capacity(size_t capacity_needed);
};

constexpr bool contains(FsStr str, FsStr substr) {
  return str.find(substr) != ktl::string_view::npos;
}

constexpr bool contains(FsStr str, char character) {
  return str.find(character) != ktl::string_view::npos;
}

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_PATH_H_

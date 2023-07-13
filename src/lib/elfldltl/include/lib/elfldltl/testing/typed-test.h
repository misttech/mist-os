// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TESTING_TYPED_TEST_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TESTING_TYPED_TEST_H_

#include <lib/elfldltl/layout.h>

#include <gtest/gtest.h>

namespace elfldltl::testing {

using AllFormatsTypedTest = elfldltl::AllFormats<::testing::Types>;

template <class ElfLayout>
struct FormatTypedTest : public ::testing::Test {
  using Elf = ElfLayout;
};

#define FORMAT_TYPED_TEST_SUITE(Name)                   \
  template <class Elf>                                  \
  using Name = elfldltl::testing::FormatTypedTest<Elf>; \
  TYPED_TEST_SUITE(Name, elfldltl::testing::AllFormatsTypedTest)

}  // namespace elfldltl::testing

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TESTING_TYPED_TEST_H_

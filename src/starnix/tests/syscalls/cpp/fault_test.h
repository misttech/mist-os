// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_SYSCALLS_CPP_FAULT_TEST_H_
#define SRC_STARNIX_TESTS_SYSCALLS_CPP_FAULT_TEST_H_

#include <sys/mman.h>

#include <gtest/gtest.h>

template <typename T>
class FaultTest : public T {
 protected:
  static void SetUpTestSuite() {
    faulting_ptr_ = mmap(nullptr, kFaultingSize_, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    ASSERT_NE(faulting_ptr_, MAP_FAILED);
  }

  static void TearDownTestSuite() {
    EXPECT_EQ(munmap(faulting_ptr_, kFaultingSize_), 0) << strerror(errno);
    faulting_ptr_ = nullptr;
  }

  static constexpr size_t kFaultingSize_ = 987;
  static inline void* faulting_ptr_;
};

#endif  // SRC_STARNIX_TESTS_SYSCALLS_CPP_FAULT_TEST_H_

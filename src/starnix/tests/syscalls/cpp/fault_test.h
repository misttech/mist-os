// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_SYSCALLS_CPP_FAULT_TEST_H_
#define SRC_STARNIX_TESTS_SYSCALLS_CPP_FAULT_TEST_H_

#include <sys/mman.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

class FaultTest : public testing::Test {
 protected:
  static void SetUpTestSuite();
  static void TearDownTestSuite();

  static constexpr size_t kFaultingSize_ = 987;
  static void* faulting_ptr_;
};

class FaultFileTest : public FaultTest, public testing::WithParamInterface<int (*)()> {
 protected:
  void SetUp() override;

  void TearDown() override;

  void SetFdNonBlocking();

  const fbl::unique_fd& fd() { return fd_; }

 private:
  fbl::unique_fd fd_;
};

#endif  // SRC_STARNIX_TESTS_SYSCALLS_CPP_FAULT_TEST_H_

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ld-load-zircon-process-tests-base.h"

#include <gtest/gtest.h>

namespace ld::testing {

const char* LdLoadZirconProcessTestsBase::process_name() const {
  return ::testing::UnitTest::GetInstance()->current_test_info()->name();
}

void LdLoadZirconProcessTestsBase::set_process(zx::process process) {
  ASSERT_FALSE(process_);
  process_ = std::move(process);
}

int64_t LdLoadZirconProcessTestsBase::Wait() {
  int64_t result = -1;

  auto wait_for_termination = [this, &result]() {
    zx_signals_t signals;
    ASSERT_EQ(process_.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), &signals), ZX_OK);
    ASSERT_TRUE(signals & ZX_PROCESS_TERMINATED);
    zx_info_process_t info;
    ASSERT_EQ(process_.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr), ZX_OK);
    ASSERT_TRUE(info.flags & ZX_INFO_PROCESS_FLAG_STARTED);
    ASSERT_TRUE(info.flags & ZX_INFO_PROCESS_FLAG_EXITED);
    result = info.return_code;
  };
  wait_for_termination();

  return result;
}

LdLoadZirconProcessTestsBase::~LdLoadZirconProcessTestsBase() {
  if (process_) {
    EXPECT_EQ(process_.kill(), ZX_OK);
  }
}

}  // namespace ld::testing

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_TEST_CHDIR_GUARD_H_
#define LIB_LD_TEST_TEST_CHDIR_GUARD_H_

namespace ld::testing {

class TestChdirGuard {
 public:
  explicit TestChdirGuard();
  ~TestChdirGuard();

 private:
  int cwd_ = -1;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_TEST_CHDIR_GUARD_H_

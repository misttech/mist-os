// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>

#include <gtest/gtest.h>

namespace {

TEST(IdleTest, Idle4Mins) {
  for (int i = 0; i < 4; i++) {
    std::cout << "1 minute sleep (" << i + 1 << "/4)..." << std::endl;
    sleep(60);
  }
  std::cout << "Wake up!" << std::endl;
}

}  // namespace

int main(int argc, char** argv) {
  testing::InitGoogleTest(&argc, argv);
  return RUN_ALL_TESTS();
}

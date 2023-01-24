// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdio.h>

#include "test_observer.h"

int main(int argc, char** argv) {
  setlinebuf(stdout);
  zxtest::Runner::GetInstance()->AddObserver(&test_observer);

  return RUN_ALL_TESTS(argc, argv);
}

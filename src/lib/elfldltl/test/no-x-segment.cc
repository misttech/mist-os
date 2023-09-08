// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/compiler.h>

#include <array>

#include "test-data.h"

const int rodata = 5;
int bss[BSS_SIZE];

__CONSTINIT auto data = []() constexpr {
  std::array<int, DATA_SIZE + 2> ret{18};
  ret.back() = 1;
  return ret;
}();

extern "C" __CONSTINIT const TestData test_data{&rodata, &data.front(), bss};

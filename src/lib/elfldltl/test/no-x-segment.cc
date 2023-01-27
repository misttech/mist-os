// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <array>

#include "test-data.h"

#if __cplusplus >= 202002L
#define CONSTINIT constinit
#elif defined(__clang__)
#define CONSTINIT [[clang::require_constant_initialization]]
#elif defined(__GNUC__)
#define CONSTINIT __constinit
#else
#error "Unknown compiler"
#endif

const int rodata = 5;
int bss[BSS_SIZE];

CONSTINIT auto data = []() constexpr {
  std::array<int, DATA_SIZE + 2> ret{18};
  ret.back() = 1;
  return ret;
}();

extern "C" CONSTINIT const TestData test_data{&rodata, &data.front(), bss};

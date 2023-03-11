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

extern "C" CONSTINIT const RelroData relro_data{.relocated = &rodata};

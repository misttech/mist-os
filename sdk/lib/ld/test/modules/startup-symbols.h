// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_MODULES_STARTUP_SYMBOLS_H_
#define LIB_LD_TEST_MODULES_STARTUP_SYMBOLS_H_

#include <stdint.h>

// This file contains symbols decls that are called directly by tests in
// //sdk/lib/dl/test:unittests. These symbols should be provided by targets that
// serve as startup modules for those tests.
extern "C" [[gnu::visibility("default")]] int64_t foo_v1_StartupModulesBasic();
extern "C" [[gnu::visibility("default")]] int64_t foo_v2_StartupModulesBasic();

extern "C" [[gnu::visibility("default")]] int64_t foo_v1_StartupModulesPriorityOverGlobal();
extern "C" [[gnu::visibility("default")]] int64_t call_foo_v1_StartupModulesPriorityOverGlobal();

#endif  // LIB_LD_TEST_MODULES_STARTUP_SYMBOLS_H_

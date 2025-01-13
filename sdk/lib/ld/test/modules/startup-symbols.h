// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_MODULES_STARTUP_SYMBOLS_H_
#define LIB_LD_TEST_MODULES_STARTUP_SYMBOLS_H_

#include <stdint.h>

#include <vector>

// This file contains symbols decls that are called directly by tests in
// //sdk/lib/dl/test:unittests. These symbols should be provided by targets that
// serve as startup modules for those tests.
extern "C" {

[[gnu::visibility("default")]] int64_t foo_v1_StartupModulesBasic();
[[gnu::visibility("default")]] int64_t foo_v2_StartupModulesBasic();

[[gnu::visibility("default")]] int64_t foo_v1_StartupModulesPriorityOverGlobal();
[[gnu::visibility("default")]] int64_t call_foo_v1_StartupModulesPriorityOverGlobal();

[[gnu::visibility("default")]] int* get_static_tls_var();

[[gnu::visibility("default"),
  gnu::tls_model("global-dynamic")]] extern thread_local int gStaticTlsVar;
}

// This is a class interface for test modules to register that their initializer
// and finalizer functions have run.
class RegisterInitFini {
 public:
  // Test module init and fini functions call these Register* methods with a
  // value specific to that function in the test module.
  virtual void RegisterInit(int) = 0;
  virtual void RegisterFini(int) = 0;
};

// Tests in //sdk/lib/dl/test/dl-load-tests-initfini.cc will set this global to
// an instantiated object that defines the Register* methods that test modules
// will use to register their init/fini functions.
[[gnu::visibility("default")]] extern RegisterInitFini* gRegisterInitFini;

constexpr int kStaticTlsDataValue = 16;

#endif  // LIB_LD_TEST_MODULES_STARTUP_SYMBOLS_H_

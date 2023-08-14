// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TEST_LD_STARTUP_SPAWN_PROCESS_TESTS_POSIX_H_
#define LIB_LD_TEST_LD_STARTUP_SPAWN_PROCESS_TESTS_POSIX_H_

#include <cstdint>
#include <initializer_list>
#include <string>
#include <vector>

#include <gtest/gtest.h>

#include "ld-load-tests-base.h"

// The spawned-process tests work by using the normal system program loader
// such that it loads the executable and finds the dynamic linker as its
// PT_INTERP in the standard way.

namespace ld::testing {

// On POSIX-like systems this means using posix_spawn.  There is just one big
// operation, really.  So all the methods before Run() just collect details of
// what to do.
class LdStartupSpawnProcessTests : public ::testing::Test, public LdLoadTestsBase {
 public:
  void Init(std::initializer_list<std::string_view> args = {});

  void Load(std::string_view executable_name);

  int64_t Run();

  ~LdStartupSpawnProcessTests();

 private:
  std::string executable_;
  std::vector<std::string> argv_;
  pid_t pid_ = -1;
};

}  // namespace ld::testing

#endif  // LIB_LD_TEST_LD_STARTUP_SPAWN_PROCESS_TESTS_POSIX_H_

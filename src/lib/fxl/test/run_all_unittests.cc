// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include <lib/syslog/cpp/macros.h>

#include <gtest/gtest.h>
#ifdef __Fuchsia__
#include <lib/async-loop/cpp/loop.h>
#endif

#include "test_settings.h"

int main(int argc, char** argv) {
#ifdef __Fuchsia__
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  loop.StartThread("test-interest-listener-thread");
  if (!fxl::SetTestSettings(argc, argv, loop.dispatcher())) {
    FX_LOGS(ERROR) << "Failed to parse log settings from command-line";
    return EXIT_FAILURE;
  }
#else
  if (!fxl::SetTestSettings(argc, argv)) {
    FX_LOGS(ERROR) << "Failed to parse log settings from command-line";
    return EXIT_FAILURE;
  }
#endif

  // Setting this flag to true causes googletest to *generate* and log the random seed.
  GTEST_FLAG_SET(shuffle, true);

  testing::InitGoogleTest(&argc, argv);
  return RUN_ALL_TESTS();
}

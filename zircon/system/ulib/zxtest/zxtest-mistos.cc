// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/console.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <zxtest/zxtest.h>

int zx_run_all_test(int argc, const cmd_args* argv, uint32_t flags) {
  return RUN_ALL_TESTS(0, nullptr);
}

STATIC_COMMAND_START
STATIC_COMMAND("zx_run_all_test", "Run all registered zxtest(unittesting)", &zx_run_all_test)
STATIC_COMMAND_END(zxtest)

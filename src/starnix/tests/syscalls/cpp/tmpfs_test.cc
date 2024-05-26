// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdlib.h>

#include <gtest/gtest.h>

#include "fault_test.h"
#include "fault_test_suite.h"

namespace {

int CreateTempFile() {
  char tmpl[] = "/tmp/tmpfile.XXXXXX";
  return mkstemp(tmpl);
}

INSTANTIATE_TEST_SUITE_P(TmpfsFaultTest, FaultFileTest, ::testing::Values(CreateTempFile));

}  // namespace

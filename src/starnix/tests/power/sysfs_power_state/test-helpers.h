// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_POWER_SYSFS_POWER_STATE_TEST_HELPERS_H_
#define SRC_STARNIX_TESTS_POWER_SYSFS_POWER_STATE_TEST_HELPERS_H_

#include <string>

void DoTest(const std::string& file_to_increment, const std::string& file_to_not_change,
            bool expect_success);

#endif  // SRC_STARNIX_TESTS_POWER_SYSFS_POWER_STATE_TEST_HELPERS_H_

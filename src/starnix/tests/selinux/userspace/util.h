// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_
#define SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_

#include <string.h>

#include <string>

#define ASSERT_SUCCESS(x) ASSERT_NE((x), -1) << "Failed: " #x << ": " << strerror(errno)

// Loads the policy |name|.
void LoadPolicy(const std::string& name);

// Atomically writes |contents| to |file|, and fails the test otherwise.
void WriteContents(const std::string& file, const std::string& contents, bool create = false);

// Reads |file|, or fail the test.
std::string ReadFile(const std::string& file);

#endif  // SRC_STARNIX_TESTS_SELINUX_USERSPACE_UTIL_H_

// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/stdformat/print.h>

#include "sdk/lib/stdcompat/test/gtest.h"

namespace {

TEST(PrintTest, Print) { cpp23::print("Hello {}: {}", "World", 42); }

TEST(PrintTest, Println) { cpp23::println("Hello {}: {}", "World", 42); }

TEST(PrintTest, PrintToFile) {
  char buffer[100] = "Old Text";
  FILE *fp;

  fp = fmemopen(buffer, sizeof(buffer), "w");
  cpp23::print(fp, "Hello {}: {}", "World", 42);
  fclose(fp);
  ASSERT_STREQ(buffer, "Hello World: 42");
}

TEST(PrintTest, PrintlnToFile) {
  char buffer[100] = "Old Text";
  FILE *fp;

  fp = fmemopen(buffer, sizeof(buffer), "w");
  cpp23::println(fp, "Hello {}: {}", "World", 42);
  fclose(fp);
  ASSERT_STREQ(buffer, "Hello World: 42\n");
}

}  // namespace

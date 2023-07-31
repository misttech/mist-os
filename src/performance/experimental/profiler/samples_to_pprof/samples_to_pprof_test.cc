// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "samples_to_pprof.h"

#include <cstdint>
#include <optional>
#include <string>

#include <gtest/gtest.h>

TEST(SamplesToPprofTest, ParseBackTrace) {
  const std::string backtrace =
      "#0    0x000001859ae0e03c in count(int) ../../src/performance/experimental/profiler/tests/demo_target/main.cc:16 <<VMO#34649=blob-909e53e9>>+0x103c";
  const std::string space_in_func_name =
      "#0    0x000001bc5b25c562 in async_loop_post_task(async_dispatcher_t*, async_task_t*) ../../zircon/system/ulib/async-loop/loop.c:587 <<VMO#1357815=blob-0bed55f6>>+0x7f562";
  const std::string no_func_name =
      "#0    0x000041711e54a029 in fidling/gen/zircon/vdso/zx/zither/kernel/lib/syscalls/syscalls.inc:738 <libzircon.so>+0x9029";
  const std::string long_func_name =
      "#0    0x000001bc5b20466c in std::__2::basic_string<char, std::__2::char_traits<char>, std::__2::allocator<char>>::size(const std::__2::basic_string<char, std::__2::char_traits<char>, std::__2::allocator<char> >*) ../../prebuilt/third_party/clang/linux-x64/include/c++/v1/string:1138 <<VMO#1357815=blob-0bed55f6>>+0x2766c";

  std::optional<BackTraceEntry> backtrace_parsed = parseBackTraceEntry(backtrace);
  ASSERT_TRUE(backtrace_parsed.has_value());
  EXPECT_EQ(backtrace_parsed->addr, uint64_t{0x000001859ae0e03c});
  EXPECT_EQ(backtrace_parsed->function_name, "count(int)");
  EXPECT_EQ(backtrace_parsed->file_name,
            "../../src/performance/experimental/profiler/tests/demo_target/main.cc");
  EXPECT_EQ(backtrace_parsed->line_no, 16);

  std::optional<BackTraceEntry> space_in_func_name_parsed = parseBackTraceEntry(space_in_func_name);
  ASSERT_TRUE(space_in_func_name_parsed.has_value());

  EXPECT_EQ(space_in_func_name_parsed->addr, uint64_t{0x000001bc5b25c562});
  EXPECT_EQ(space_in_func_name_parsed->function_name,
            "async_loop_post_task(async_dispatcher_t*, async_task_t*)");
  EXPECT_EQ(space_in_func_name_parsed->file_name, "../../zircon/system/ulib/async-loop/loop.c");
  EXPECT_EQ(space_in_func_name_parsed->line_no, 587);

  std::optional<BackTraceEntry> no_func_name_parsed = parseBackTraceEntry(no_func_name);
  ASSERT_TRUE(no_func_name_parsed.has_value());

  EXPECT_EQ(no_func_name_parsed->addr, uint64_t{0x000041711e54a029});
  EXPECT_EQ(no_func_name_parsed->function_name, "");
  EXPECT_EQ(no_func_name_parsed->file_name,
            "fidling/gen/zircon/vdso/zx/zither/kernel/lib/syscalls/syscalls.inc");
  EXPECT_EQ(no_func_name_parsed->line_no, 738);

  std::optional<BackTraceEntry> long_func_name_parsed = parseBackTraceEntry(long_func_name);
  ASSERT_TRUE(long_func_name_parsed.has_value());
  EXPECT_EQ(long_func_name_parsed->addr, uint64_t{0x000001bc5b20466c});
  EXPECT_EQ(
      long_func_name_parsed->function_name,
      "std::__2::basic_string<char, std::__2::char_traits<char>, std::__2::allocator<char>>::size(const std::__2::basic_string<char, std::__2::char_traits<char>, std::__2::allocator<char> >*)");
  EXPECT_EQ(long_func_name_parsed->file_name,
            "../../prebuilt/third_party/clang/linux-x64/include/c++/v1/string");
  EXPECT_EQ(long_func_name_parsed->line_no, 1138);
}

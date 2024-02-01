// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <algorithm>
#include <cinttypes>
#include <cstdint>

#include <gtest/gtest.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/lib/unwinder/unwind_local.h"

namespace unwinder {

// Declarations that might be put in a header file in the future.
int CfiOnly(std::function<int()>);
int FpOnly(std::function<int()>);
int ScsOnly(std::function<int()>);

namespace {

// Use global variables to avoid captures in lambdas.
std::vector<Frame> frames;
std::vector<uint64_t> expected_pcs;
uint64_t test_start_pc;

// Call a sequence of functions recursively and record the return addresses in |expected_pcs|
// At the end, unwind the stack using |UnwindLocal()| and save to |frames|. For example,
// `call_sequence(FpOnly, ScsOnly)` expands to `FpOnly([]() { ScsOnly([]() { UnwindLocal() }); });`
template <typename... FN>
int call_sequence() {
  frames = UnwindLocal();
  return 0;
}

template <typename F1, typename... FN>
int call_sequence(F1 f1, FN... fn) {
  return f1([=]() {
    int res = call_sequence(fn...);
    expected_pcs.push_back(reinterpret_cast<uint64_t>(__builtin_return_address(0)));
    return res;
  });
}

// Check whether |expected_pcs| is a subsequence of |frames|.
void check_stack() {
  auto expected_it = expected_pcs.begin();
  for (auto& frame : frames) {
    if (uint64_t pc; frame.regs.GetPC(pc).ok() && pc == *expected_it) {
      expected_it++;
      if (expected_it == expected_pcs.end()) {
        break;
      }
    }
  }
  if (expected_it != expected_pcs.end()) {
    std::string msg = "Expect to find the following frames:\n";
    for (auto& pc : expected_pcs) {
      msg += fxl::StringPrintf("%#" PRIx64 "\n", pc);
    }
    msg += "But actually get:\n";
    for (auto& frame : frames) {
      msg += frame.Describe() + "\n";
    }
    FAIL() << msg;
  }
}

template <typename F1, typename... FN>
[[gnu::noinline]] int TestStart(F1 f1, FN... fn) {
  frames.clear();
  expected_pcs.clear();
  test_start_pc = 0;

  int res = call_sequence(f1, fn...);
  test_start_pc = reinterpret_cast<uint64_t>(__builtin_return_address(0));
  expected_pcs.push_back(test_start_pc);
  return res;
}

size_t GetTestStartFrameIndex() {
  size_t start_idx = 0;
  for (auto& frame : frames) {
    if (uint64_t pc; frame.regs.GetPC(pc).ok() && pc == test_start_pc) {
      return start_idx;
    }
    start_idx++;
  }
  return -1;
}

// It should be possible to interoperate between FP unwinder and CFI unwinder.
TEST(Unwinder, HybridUnwinding) {
  int res = TestStart(FpOnly, FpOnly, CfiOnly, FpOnly);

  ASSERT_EQ(res, 31);

  // FpOnly, CfiOnly, FpOnly, FpOnly could be the first four frames before the TestStart().
  EXPECT_GE(GetTestStartFrameIndex(), 4ul);

  check_stack();
}

#if __has_feature(shadow_call_stack)
TEST(Unwinder, Scs) {
  // SCS unwinder cannot be combined with other unwinders, e.g.
  // (CFI -> SCS means unwinding using CFI first, then using SCS.)
  //  * CFI -> SCS will work, because CFI also recovers x18.
  //  * FP -> SCS won't work, because FP doesn't recover x18.
  //  * SCS -> FP and SCS -> CFI won't work, because SCS only recovers PC and x18.
  int res = TestStart(ScsOnly, ScsOnly, ScsOnly, CfiOnly);

  ASSERT_EQ(res, 301);
  // CfiOnly, ScsOnly, ScsOnly, ScsOnly could be the first four frames before the TestStart().
  ASSERT_GE(GetTestStartFrameIndex(), 4ul);

  check_stack();
}
#endif

}  // namespace

}  // namespace unwinder

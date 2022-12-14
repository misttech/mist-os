// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/developer/debug/e2e_tests/e2e_test.h"
#include "src/developer/debug/zxdb/client/breakpoint.h"

namespace zxdb {

namespace {

// This test conditionally breaks in "loop.cc" on  line 9 if i > 2.
class ConditionalBreakpoint : public E2eTest {
 public:
  void Run() {
    console().ProcessInputLine(
        "break loop.cc:9 if i > 2",
        fxl::MakeRefCounted<ConsoleCommandContext>(&console(), [this](const Err& first) {
          ASSERT_TRUE(first.ok());

          // Make sure the breakpoint was properly initialized with the condition.
          bp_ = console().context().GetActiveBreakpoint();
          EXPECT_NE(bp_, nullptr);
          EXPECT_STREQ(bp_->GetSettings().condition.c_str(), "i > 2");
        }));
    console().ProcessInputLine(
        "run-component fuchsia-pkg://fuchsia.com/zxdb_e2e_inferiors#meta/loop.cm");
    loop().Run();
  }

 private:
  void OnThreadStopped(Thread* thread, const StopInfo& info) override {
    FX_LOGS(INFO) << "OnThreadStopped stopped_count_ = " << stopped_count_;
    EXPECT_EQ(info.hit_breakpoints.size(), 1u);
    EXPECT_EQ(info.hit_breakpoints[0].get(), bp_);
    stopped_count_++;
    console().ProcessInputLine("continue");
  }

  void WillDestroyProcess(Process* process, DestroyReason reason, int exit_code,
                          uint64_t timestamp) override {
    ASSERT_NE(bp_, nullptr);
    EXPECT_EQ(exit_code, static_cast<int>(DestroyReason::kExit));
    // Conditional breakpoints "hit count" is really counting the number of times the expression is
    // evaluated.
    EXPECT_EQ(bp_->GetStats().hit_count, 5u);
    // Should have gotten 2 "stopped" notifications when i == 3 and i == 4 in loop.cc.
    EXPECT_EQ(stopped_count_, 2u);
    loop().QuitNow();
  }

  uint32_t stopped_count_ = 0;
  Breakpoint* bp_ = nullptr;
};

}  // namespace

// TODO(fxbug.dev/117067): fix the flake and reenable the test.
TEST_F(ConditionalBreakpoint, DISABLED_ConditionalBreakpoint) { Run(); }

}  // namespace zxdb

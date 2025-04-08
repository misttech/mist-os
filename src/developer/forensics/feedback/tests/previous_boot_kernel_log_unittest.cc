// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachments/previous_boot_kernel_log.h"

#include <lib/async/cpp/executor.h>

#include <gtest/gtest.h>

#include "src/developer/forensics/testing/gmatchers.h"
#include "src/developer/forensics/testing/gpretty_printers.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"

namespace forensics::feedback {
namespace {

class PreviousBootKernelLogTest : public UnitTestFixture {
 public:
  PreviousBootKernelLogTest() : executor_(dispatcher()) {
    redactor_ = std::make_unique<IdentityRedactor>(inspect::BoolProperty());
  }

  async::Executor& GetExecutor() { return executor_; }

  void SetRedactor(std::unique_ptr<RedactorBase> redactor) { redactor_ = std::move(redactor); }
  RedactorBase* GetRedactor() { return redactor_.get(); }

 private:
  async::Executor executor_;
  std::unique_ptr<RedactorBase> redactor_;
};

// Redacts everything.
class SimpleRedactor : public RedactorBase {
 public:
  SimpleRedactor() : RedactorBase(inspect::BoolProperty()) {}

  std::string& Redact(std::string& text) override {
    text = "<REDACTED>";
    return text;
  }

  std::string& RedactJson(std::string& text) override { return Redact(text); }

  std::string UnredactedCanary() const override { return ""; }
  std::string RedactedCanary() const override { return ""; }
};

TEST_F(PreviousBootKernelLogTest, GetReturnsMissingValue) {
  PreviousBootKernelLog provider(/*dlog=*/std::nullopt, GetRedactor());

  AttachmentValue attachment(Error::kNotSet);
  GetExecutor().schedule_task(
      provider.Get(/*ticket=*/123)
          .and_then([&attachment](AttachmentValue& res) { attachment = std::move(res); })
          .or_else([] { FX_LOGS(FATAL) << "Logic error"; }));

  RunLoopUntilIdle();
  EXPECT_THAT(attachment, AttachmentValueIs(Error::kMissingValue));
}

TEST_F(PreviousBootKernelLogTest, GetReturnsDlog) {
  PreviousBootKernelLog provider(/*dlog=*/"test dlog", GetRedactor());

  AttachmentValue attachment(Error::kNotSet);
  GetExecutor().schedule_task(
      provider.Get(/*ticket=*/123)
          .and_then([&attachment](AttachmentValue& res) { attachment = std::move(res); })
          .or_else([] { FX_LOGS(FATAL) << "Logic error"; }));

  RunLoopUntilIdle();
  EXPECT_THAT(attachment, AttachmentValueIs("test dlog"));
}

TEST_F(PreviousBootKernelLogTest, DlogIsRedacted) {
  SetRedactor(std::make_unique<SimpleRedactor>());
  PreviousBootKernelLog provider(/*dlog=*/"test dlog", GetRedactor());

  AttachmentValue attachment(Error::kNotSet);
  GetExecutor().schedule_task(
      provider.Get(/*ticket=*/123)
          .and_then([&attachment](AttachmentValue& res) { attachment = std::move(res); })
          .or_else([] { FX_LOGS(FATAL) << "Logic error"; }));

  RunLoopUntilIdle();
  EXPECT_THAT(attachment, AttachmentValueIs("<REDACTED>"));
}

TEST_F(PreviousBootKernelLogTest, ForceCompletionIsNoOp) {
  PreviousBootKernelLog provider(/*dlog=*/"test dlog", GetRedactor());

  AttachmentValue attachment(Error::kNotSet);
  GetExecutor().schedule_task(
      provider.Get(/*ticket=*/123)
          .and_then([&attachment](AttachmentValue& res) { attachment = std::move(res); })
          .or_else([] { FX_LOGS(FATAL) << "Logic error"; }));

  // Should be a no-op. The promise is immediately ready and is just waiting to be executed.
  provider.ForceCompletion(123, Error::kDefault);
  EXPECT_THAT(attachment, AttachmentValueIs(Error::kNotSet));
}

}  // namespace
}  // namespace forensics::feedback

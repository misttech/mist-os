// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/reboot_log/graceful_reboot_reason.h"

#include <vector>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/forensics/testing/gpretty_printers.h"
#include "src/developer/forensics/testing/stubs/cobalt_logger_factory.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/lib/files/scoped_temp_dir.h"
#include "src/lib/timekeeper/test_clock.h"

namespace forensics {
namespace feedback {
namespace {

TEST(GracefulRebootReasonTest, VerifyContentConversion) {
  // ToFileContent() & FromFileContent() for reboot reasons from |power::statecontrol::RebootReason|
  // should be reversible.

  const std::vector<GracefulRebootReason> reasons = {
      GracefulRebootReason::kUserRequest,
      GracefulRebootReason::kSystemUpdate,
      GracefulRebootReason::kRetrySystemUpdate,
      GracefulRebootReason::kHighTemperature,
      GracefulRebootReason::kSessionFailure,
      GracefulRebootReason::kSysmgrFailure,
      GracefulRebootReason::kCriticalComponentFailure,
      GracefulRebootReason::kFdr,
      GracefulRebootReason::kZbiSwap,
      GracefulRebootReason::kNotSupported,
      GracefulRebootReason::kNetstackMigration,
  };

  for (const auto reason : reasons) {
    EXPECT_THAT(FromFileContent(ToFileContent({reason})), testing::ElementsAre(reason));
  }
}

TEST(GracefulRebootReasonTest, VerifyContentConversionWithMultipleReasons) {
  // ToFileContent() & FromFileContent() for reboot reasons from |power::statecontrol::RebootReason|
  // should be reversible when there are multiple reasons.

  const std::vector<GracefulRebootReason> reasons = {
      GracefulRebootReason::kUserRequest,
      GracefulRebootReason::kSystemUpdate,
      GracefulRebootReason::kRetrySystemUpdate,
      GracefulRebootReason::kHighTemperature,
      GracefulRebootReason::kSessionFailure,
      GracefulRebootReason::kSysmgrFailure,
      GracefulRebootReason::kCriticalComponentFailure,
      GracefulRebootReason::kFdr,
      GracefulRebootReason::kZbiSwap,
      GracefulRebootReason::kNotSupported,
      GracefulRebootReason::kNetstackMigration,
  };

  // Verify all reasons at once.
  EXPECT_THAT(FromFileContent(ToFileContent(reasons)), testing::ElementsAreArray(reasons));
}

TEST(GracefulRebootReasonTest, VerifyContentConversionWithNoReasons) {
  // ToFileContent() & FromFileContent() for reboot reasons from |power::statecontrol::RebootReason|
  // should be reversible when there are no reasons.
  EXPECT_TRUE(FromFileContent(ToFileContent({})).empty());
}

constexpr char kFilename[] = "graceful_reboot_reason.txt";

struct TestParam {
  std::string test_name;
  GracefulRebootReason input_reboot_reason;
  std::string output_reason;
};

class WriteGracefulRebootReasonTest : public UnitTestFixture,
                                      public testing::WithParamInterface<TestParam> {
 public:
  WriteGracefulRebootReasonTest() : cobalt_(dispatcher(), services(), &clock_) {}

 protected:
  std::string Path() { return files::JoinPath(tmp_dir_.path(), kFilename); }

  timekeeper::TestClock clock_;
  cobalt::Logger cobalt_;

 private:
  files::ScopedTempDir tmp_dir_;
};

INSTANTIATE_TEST_SUITE_P(WithVariousRebootReasons, WriteGracefulRebootReasonTest,
                         ::testing::ValuesIn(std::vector<TestParam>({
                             {
                                 "UserRequest",
                                 GracefulRebootReason::kUserRequest,
                                 "USER REQUEST",
                             },
                             {
                                 "SystemUpdate",
                                 GracefulRebootReason::kSystemUpdate,
                                 "SYSTEM UPDATE",
                             },
                             {
                                 "RetrySystemUpdate",
                                 GracefulRebootReason::kRetrySystemUpdate,
                                 "RETRY SYSTEM UPDATE",
                             },
                             {
                                 "HighTemperature",
                                 GracefulRebootReason::kHighTemperature,
                                 "HIGH TEMPERATURE",
                             },
                             {
                                 "SessionFailure",
                                 GracefulRebootReason::kSessionFailure,
                                 "SESSION FAILURE",
                             },
                             {
                                 "SystemFailure",
                                 GracefulRebootReason::kSysmgrFailure,
                                 "SYSMGR FAILURE",
                             },
                             {
                                 "CriticalComponentFailure",
                                 GracefulRebootReason::kCriticalComponentFailure,
                                 "CRITICAL COMPONENT FAILURE",
                             },
                             {
                                 "FactoryDataReset",
                                 GracefulRebootReason::kFdr,
                                 "FACTORY DATA RESET",
                             },
                             {
                                 "ZbiSwap",
                                 GracefulRebootReason::kZbiSwap,
                                 "ZBI SWAP",
                             },
                             {
                                 "OutOfMemory",
                                 GracefulRebootReason::kOutOfMemory,
                                 "OUT OF MEMORY",
                             },
                             {
                                 "NetstackMigration",
                                 GracefulRebootReason::kNetstackMigration,
                                 "NETSTACK MIGRATION",
                             },
                             {
                                 "NotSupported",
                                 static_cast<GracefulRebootReason>(100u),
                                 "NOT SUPPORTED",
                             },
                         })),
                         [](const testing::TestParamInfo<TestParam>& info) {
                           return info.param.test_name;
                         });

TEST_P(WriteGracefulRebootReasonTest, Succeed) {
  const auto param = GetParam();

  SetUpCobaltServer(std::make_unique<stubs::CobaltLoggerFactory>());

  WriteGracefulRebootReasons({param.input_reboot_reason}, &cobalt_, Path());

  std::string contents;
  ASSERT_TRUE(files::ReadFileToString(Path(), &contents));
  EXPECT_EQ(contents, param.output_reason.c_str());

  RunLoopUntilIdle();
}

}  // namespace
}  // namespace feedback
}  // namespace forensics

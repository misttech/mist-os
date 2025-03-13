// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/last_reboot.h"

#include <fuchsia/feedback/cpp/fidl.h>
#include <fuchsia/hardware/power/statecontrol/cpp/fidl.h>
#include <lib/inspect/cpp/vmo/types.h>

#include <memory>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/forensics/feedback/reboot_log/reboot_log.h"
#include "src/developer/forensics/testing/stubs/cobalt_logger_factory.h"
#include "src/developer/forensics/testing/stubs/crash_reporter.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/developer/forensics/utils/cobalt/logger.h"
#include "src/lib/files/path.h"
#include "src/lib/timekeeper/async_test_clock.h"

namespace forensics::feedback {
namespace {

using ::testing::IsEmpty;
using ::testing::UnorderedElementsAreArray;

class LastRebootTest : public UnitTestFixture {
 public:
  LastRebootTest() : clock_(dispatcher()), cobalt_(dispatcher(), services(), &clock_) {
    SetUpCobaltServer(std::make_unique<stubs::CobaltLoggerFactory>());
  }

  void TearDown() override {
    files::DeletePath("/tmp/has_reported_on_reboot_log.txt", /*recursive=*/false);
  }

 protected:
  void SetUpCrashReporterServer(std::unique_ptr<stubs::CrashReporterBase> crash_reporter_server) {
    crash_reporter_server_ = std::move(crash_reporter_server);
  }

  cobalt::Logger* Cobalt() { return &cobalt_; }
  RedactorBase* Redactor() { return &redactor_; }

  fuchsia::feedback::CrashReporter* CrashReporter() { return crash_reporter_server_.get(); }

 private:
  timekeeper::AsyncTestClock clock_;
  cobalt::Logger cobalt_;
  IdentityRedactor redactor_{inspect::BoolProperty()};

  std::unique_ptr<stubs::CrashReporterBase> crash_reporter_server_;
};

TEST_F(LastRebootTest, FirstInstance) {
  const zx::duration oom_crash_reporting_delay = zx::sec(90);
  const RebootLog reboot_log(RebootReason::kOOM, "reboot log", /*last_boot_uptime=*/zx::sec(1),
                             /*last_boot_runtime=*/zx::msec(500),
                             /*critical_process=*/std::nullopt);

  SetUpCrashReporterServer(
      std::make_unique<stubs::CrashReporter>(stubs::CrashReporter::Expectations{
          .crash_signature = ToCrashSignature(reboot_log.RebootReason()),
          .reboot_log = reboot_log.RebootLogStr(),
          .uptime = reboot_log.Uptime(),
          .runtime = reboot_log.Runtime(),
          .is_fatal = IsFatal(reboot_log.RebootReason()),
      }));

  LastReboot last_reboot(dispatcher(), services(), Cobalt(), Redactor(), CrashReporter(),
                         LastReboot::Options{
                             .is_first_instance = true,
                             .reboot_log = reboot_log,
                             .oom_crash_reporting_delay = oom_crash_reporting_delay,
                         });

  RunLoopFor(oom_crash_reporting_delay);

  EXPECT_THAT(
      ReceivedCobaltEvents(),
      UnorderedElementsAreArray({
          cobalt::Event(cobalt::LastRebootReason::kSystemOutOfMemory, zx::sec(1).to_usecs()),
      }));
}

TEST_F(LastRebootTest, IsNotFirstInstance) {
  const zx::duration oom_crash_reporting_delay = zx::sec(90);
  const RebootLog reboot_log(RebootReason::kOOM, "reboot log", /*last_boot_uptime=*/zx::sec(1),
                             /*last_boot_runtime=*/zx::msec(500),
                             /*critical_process=*/std::nullopt);

  SetUpCrashReporterServer(std::make_unique<stubs::CrashReporterNoFileExpected>());

  LastReboot last_reboot(dispatcher(), services(), Cobalt(), Redactor(), CrashReporter(),
                         LastReboot::Options{
                             .is_first_instance = false,
                             .reboot_log = reboot_log,
                             .oom_crash_reporting_delay = oom_crash_reporting_delay,
                         });

  RunLoopFor(oom_crash_reporting_delay);

  EXPECT_THAT(ReceivedCobaltEvents(), IsEmpty());
}

}  // namespace
}  // namespace forensics::feedback

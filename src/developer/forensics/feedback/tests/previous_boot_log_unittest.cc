// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/attachments/previous_boot_log.h"

#include <lib/async/cpp/executor.h>

#include "src/developer/forensics/feedback/attachments/types.h"
#include "src/developer/forensics/testing/gmatchers.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/lib/files/scoped_temp_dir.h"
#include "src/lib/timekeeper/async_test_clock.h"
#include "src/lib/timekeeper/clock.h"

namespace forensics::feedback {

class PreviousBootLogTest : public UnitTestFixture {
 public:
  PreviousBootLogTest() : executor_(dispatcher()), clock_(dispatcher()) {}

 protected:
  async::Executor& GetExecutor() { return executor_; }

  timekeeper::Clock* Clock() { return &clock_; }

  std::string NewFile() {
    std::string path;
    dir_.NewTempFile(&path);
    return path;
  }

  std::string NewFile(const std::string& data) {
    std::string path;
    dir_.NewTempFileWithData(data, &path);
    return path;
  }

 private:
  async::Executor executor_;
  timekeeper::AsyncTestClock clock_;

  files::ScopedTempDir dir_;
};

TEST_F(PreviousBootLogTest, PreviousBootLogDeletedAfterDeviceUptimeThresholdReached) {
  const uint64_t kTicket = 21;
  const std::string path = NewFile();

  // Check that the file exists
  EXPECT_TRUE(files::IsFile(path));

  PreviousBootLog previous_boot_log_(dispatcher(), Clock(), zx::sec(5), path);
  previous_boot_log_.Get(kTicket);

  RunLoopFor(zx::sec(5));

  // Check that the file is deleted after 5 seconds.
  EXPECT_FALSE(files::IsFile(path));
}

TEST_F(PreviousBootLogTest, ForceCompletionCalledWhenPromiseIsIncomplete) {
  const std::string path = NewFile();
  const uint64_t kTicket = 21;

  PreviousBootLog previous_boot_log_(dispatcher(), Clock(), zx::sec(5), path);

  AttachmentValue attachment(Error::kNotSet);
  GetExecutor().schedule_task(
      previous_boot_log_.Get(kTicket)
          .and_then([&attachment](AttachmentValue& res) { attachment = std::move(res); })
          .or_else([] { FX_LOGS(FATAL) << "Logic error"; }));

  previous_boot_log_.ForceCompletion(kTicket, Error::kDefault);

  EXPECT_TRUE(files::IsFile(path));
}

TEST_F(PreviousBootLogTest, NoPreviousBootLog) {
  // Create a file even though we're testing what happens when PreviousBootLog thinks there's no
  // file. This will let us ensure PreviousBootLog doesn't attempt to delete the file.
  const std::string path = NewFile();
  const uint64_t kTicket = 21;

  EXPECT_TRUE(files::IsFile(path));

  PreviousBootLog previous_boot_log_(dispatcher(), Clock(),
                                     /*delete_previous_boot_log_at=*/std::nullopt, path);
  previous_boot_log_.Get(kTicket);

  // Arbitrarily run for 25 hours.
  RunLoopFor(zx::hour(25));

  EXPECT_TRUE(files::IsFile(path));
}

}  // namespace forensics::feedback

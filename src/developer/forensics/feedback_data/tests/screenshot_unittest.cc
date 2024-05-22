// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback_data/screenshot.h"

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <deque>
#include <memory>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/forensics/testing/stubs/screenshot.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"

namespace forensics {
namespace feedback_data {
namespace {

using testing::UnorderedElementsAreArray;

class TakeScreenshotTest : public UnitTestFixture {
 public:
  TakeScreenshotTest() : executor_(dispatcher()) {}

 protected:
  void SetUpScreenshotServer(std::unique_ptr<stubs::ScreenshotBase> server) {
    screenshot_server_ = std::move(server);
    if (screenshot_server_) {
      InjectServiceProvider(screenshot_server_.get());
    }
  }

  ::fpromise::result<ScreenshotData, Error> TakeScreenshot(
      const zx::duration timeout = zx::sec(1)) {
    ::fpromise::result<ScreenshotData, Error> result;
    executor_.schedule_task(
        feedback_data::TakeScreenshot(dispatcher(), services(),
                                      fuchsia::ui::composition::ScreenshotFormat::PNG, timeout)
            .then([&result](::fpromise::result<ScreenshotData, Error>& res) {
              result = std::move(res);
            }));
    RunLoopFor(timeout);
    return result;
  }

  async::Executor executor_;
  bool did_timeout_ = false;

 private:
  std::unique_ptr<stubs::ScreenshotBase> screenshot_server_;
};

TEST_F(TakeScreenshotTest, Succeed_CheckerboardScreenshot) {
  const size_t image_dim_in_px = 100;
  std::deque<fuchsia::ui::composition::ScreenshotTakeResponse> screenshot_server_responses;
  screenshot_server_responses.emplace_back(stubs::CreateFakePNGScreenshot(image_dim_in_px));
  std::unique_ptr<stubs::Screenshot> server = std::make_unique<stubs::Screenshot>();
  server->set_responses(std::move(screenshot_server_responses));
  SetUpScreenshotServer(std::move(server));

  const auto result = TakeScreenshot();

  ASSERT_TRUE(result.is_ok());
  const auto& screenshot = result.value();
  EXPECT_TRUE(screenshot.data.vmo().is_valid());
  EXPECT_EQ(static_cast<size_t>(screenshot.info.height), image_dim_in_px);
  EXPECT_EQ(static_cast<size_t>(screenshot.info.width), image_dim_in_px);
}

}  // namespace
}  // namespace feedback_data
}  // namespace forensics

// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/scenic/scenic.h"

#include <lib/sys/cpp/testing/component_context_provider.h>

#include <optional>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace {

class DisplayInfoDelegate : public scenic_impl::Scenic::GetDisplayInfoDelegateDeprecated {
 public:
  void GetDisplayInfo(fuchsia::ui::scenic::Scenic::GetDisplayInfoCallback callback) override {
    auto info = fuchsia::ui::gfx::DisplayInfo();
    callback(std::move(info));
  }

  void GetDisplayOwnershipEvent(
      fuchsia::ui::scenic::Scenic::GetDisplayOwnershipEventCallback callback) override {
    zx::event event;
    callback(std::move(event));
  }
};

}  // namespace

namespace scenic_impl {
namespace test {

class ScenicUnitTest : public gtest::TestLoopFixture {
 public:
  void SetUp() override { scenic_ = std::make_unique<Scenic>(provider_.context()); }

  sys::testing::ComponentContextProvider provider_;
  std::unique_ptr<Scenic> scenic_;
};

TEST_F(ScenicUnitTest, ScenicApiAfterDelegate) {
  bool display_info = false;
  auto display_info_callback = [&](fuchsia::ui::gfx::DisplayInfo info) { display_info = true; };

  bool screenshot = false;
  auto screenshot_callback = [&](fuchsia::ui::scenic::ScreenshotData data, bool status) {
    screenshot = true;
  };

  bool display_ownership = false;
  auto display_ownership_callback = [&](zx::event event) { display_ownership = true; };

  scenic_->GetDisplayInfo(display_info_callback);
  scenic_->TakeScreenshot(screenshot_callback);
  scenic_->GetDisplayOwnershipEvent(display_ownership_callback);

  EXPECT_TRUE(display_info);
  EXPECT_TRUE(screenshot);
  EXPECT_TRUE(display_ownership);
}

TEST_F(ScenicUnitTest, UsesFlatlandCallbackIsRun) {
  std::optional<bool> uses_flatland;
  scenic_->UsesFlatland([&uses_flatland](bool enabled) { uses_flatland = enabled; });
  EXPECT_TRUE(uses_flatland.has_value());
  EXPECT_TRUE(*uses_flatland);
}

}  // namespace test
}  // namespace scenic_impl

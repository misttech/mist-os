// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_TESTING_STUBS_SCREENSHOT_H_
#define SRC_DEVELOPER_FORENSICS_TESTING_STUBS_SCREENSHOT_H_

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl_test_base.h>

#include <cstdint>
#include <deque>

#include "src/developer/forensics/testing/stubs/fidl_server.h"

namespace forensics::stubs {

// Returns an 8-bit BGRA image of a |image_dim_in_px| x |image_dim_in_px| checkerboard, where each
// white/black region is a 10x10 pixel square.
fuchsia::ui::composition::ScreenshotTakeResponse CreateFakePNGScreenshot(uint32_t image_dim_in_px);

using ScreenshotBase = MULTI_BINDING_STUB_FIDL_SERVER(fuchsia::ui::composition, Screenshot);

class Screenshot : public ScreenshotBase {
 public:
  ~Screenshot() override;

  // |fuchsia::ui::composition::Screenshot|.
  void Take(fuchsia::ui::composition::ScreenshotTakeRequest request,
            fuchsia::ui::composition::Screenshot::TakeCallback response) override;

  //  injection and verification methods.
  void set_responses(std::deque<fuchsia::ui::composition::ScreenshotTakeResponse> responses) {
    screenshot_take_responses_ = std::move(responses);
  }

 private:
  std::deque<fuchsia::ui::composition::ScreenshotTakeResponse> screenshot_take_responses_;
};

class ScreenshotClosesConnection : public ScreenshotBase {
 public:
  // |fuchsia::ui::composition::Screenshot|.
  STUB_METHOD_CLOSES_ALL_CONNECTIONS(Take, fuchsia::ui::composition::ScreenshotTakeRequest,
                                     fuchsia::ui::composition::Screenshot::TakeCallback)
};

class ScreenshotNeverReturns : public ScreenshotBase {
 public:
  // |fuchsia::ui::composition::Screenshot|.
  STUB_METHOD_DOES_NOT_RETURN(Take, fuchsia::ui::composition::ScreenshotTakeRequest,
                              fuchsia::ui::composition::Screenshot::TakeCallback)
};

}  // namespace forensics::stubs

#endif  // SRC_DEVELOPER_FORENSICS_TESTING_STUBS_SCREENSHOT_H_

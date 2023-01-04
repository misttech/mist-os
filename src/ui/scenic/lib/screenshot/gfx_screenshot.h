// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCREENSHOT_GFX_SCREENSHOT_H_
#define SRC_UI_SCENIC_LIB_SCREENSHOT_GFX_SCREENSHOT_H_

#include <fuchsia/io/cpp/fidl.h>
#include <fuchsia/ui/composition/cpp/fidl.h>

#include "src/ui/scenic/lib/screenshot/screenshot_manager.h"
#include "src/ui/scenic/lib/screenshot/util.h"

using TakeGfxScreenshot =
    std::function<void(fuchsia::ui::scenic::Scenic::TakeScreenshotCallback callback)>;

namespace screenshot {

namespace test {
class GfxScreenshotTest;
}  // namespace test

class GfxScreenshot : public fuchsia::ui::composition::Screenshot {
 public:
  GfxScreenshot(TakeGfxScreenshot take_gfx_screenshot,
                fit::function<void(GfxScreenshot*)> destroy_instance_function);
  ~GfxScreenshot() override;

  // |fuchsia::ui::composition::Screenshot|
  void Take(fuchsia::ui::composition::ScreenshotTakeRequest params, TakeCallback callback) override;
  void TakeFile(fuchsia::ui::composition::ScreenshotTakeFileRequest params,
                TakeFileCallback callback) override;

 private:
  TakeGfxScreenshot take_gfx_screenshot_;
  fit::function<void(GfxScreenshot*)> destroy_instance_function_;

  TakeCallback take_callback_ = nullptr;
  TakeFileCallback take_file_callback_ = nullptr;

  size_t served_screenshots_next_id_ = 0;
  std::unordered_map<size_t,
                     std::pair<std::unique_ptr<vfs::VmoFile>, std::unique_ptr<async::WaitOnce>>>
      served_screenshots_;

  size_t NumCurrentServedScreenshots() { return served_screenshots_.size(); }
  friend class test::GfxScreenshotTest;

  // Should be last.
  fxl::WeakPtrFactory<GfxScreenshot> weak_factory_;
};

}  // namespace screenshot

#endif  // SRC_UI_SCENIC_LIB_SCREENSHOT_GFX_SCREENSHOT_H_

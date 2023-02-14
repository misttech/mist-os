// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback_data/screenshot.h"

#include <fuchsia/ui/composition/cpp/fidl.h>

#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/fidl_oneshot.h"

namespace forensics::feedback_data {

::fpromise::promise<ScreenshotData, Error> TakeScreenshot(
    async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> services,
    zx::duration timeout) {
  fuchsia::ui::composition::ScreenshotTakeRequest args;
  args.set_format(fuchsia::ui::composition::ScreenshotFormat::BGRA_RAW);
  return OneShotCall<fuchsia::ui::composition::Screenshot,
                     &fuchsia::ui::composition::Screenshot::Take,
                     fuchsia::ui::composition::ScreenshotTakeRequest>(dispatcher, services, timeout,
                                                                      std::move(args))
      .and_then([](fuchsia::ui::composition::ScreenshotTakeResponse& result)
                    -> ::fpromise::result<ScreenshotData, Error> {
        ScreenshotData data;
        data.info.transform = fuchsia::images::Transform::NORMAL;
        data.info.width = result.size().width;
        data.info.height = result.size().height;
        data.info.stride = data.info.width * 4;
        data.info.pixel_format = fuchsia::images::PixelFormat::BGRA_8;
        data.info.color_space = fuchsia::images::ColorSpace::SRGB;
        data.info.tiling = fuchsia::images::Tiling::LINEAR;
        data.info.alpha_format = fuchsia::images::AlphaFormat::OPAQUE;
        data.data =
            fsl::SizedVmo(std::move(*result.mutable_vmo()), data.info.height * data.info.stride);
        return ::fpromise::ok(std::move(data));
      });
}

}  // namespace forensics::feedback_data

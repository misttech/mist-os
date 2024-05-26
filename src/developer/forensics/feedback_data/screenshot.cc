// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback_data/screenshot.h"

#include <cstddef>

#include "src/developer/forensics/utils/errors.h"
#include "src/developer/forensics/utils/fidl_oneshot.h"

namespace forensics::feedback_data {

::fpromise::promise<ScreenshotData, Error> TakeScreenshot(
    async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> services,
    fuchsia::ui::composition::ScreenshotFormat format, zx::duration timeout) {
  fuchsia::ui::composition::ScreenshotTakeRequest args;
  args.set_format(format);
  return OneShotCall<fuchsia::ui::composition::Screenshot,
                     &fuchsia::ui::composition::Screenshot::Take,
                     fuchsia::ui::composition::ScreenshotTakeRequest>(dispatcher, services, timeout,
                                                                      std::move(args))
      .and_then([](fuchsia::ui::composition::ScreenshotTakeResponse& result)
                    -> ::fpromise::result<ScreenshotData, Error> {
        auto res_vmo = result.mutable_vmo();
        uint64_t vmo_size;
        res_vmo->get_size(&vmo_size);
        ScreenshotData data;
        data.info.width = result.size().width;
        data.info.height = result.size().height;
        data.data = fsl::SizedVmo(std::move(*res_vmo), vmo_size);
        return ::fpromise::ok(std::move(data));
      });
}

}  // namespace forensics::feedback_data

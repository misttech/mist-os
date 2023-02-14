// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FORENSICS_FEEDBACK_DATA_SCREENSHOT_H_
#define SRC_DEVELOPER_FORENSICS_FEEDBACK_DATA_SCREENSHOT_H_

#include <fuchsia/images/cpp/fidl.h>
#include <lib/async/dispatcher.h>
#include <lib/fpromise/promise.h>
#include <lib/sys/cpp/service_directory.h>
#include <lib/zx/time.h>

#include <memory>

#include "src/developer/forensics/utils/errors.h"
#include "src/lib/fsl/vmo/sized_vmo.h"

namespace forensics::feedback_data {

struct ScreenshotData {
  fsl::SizedVmo data;
  fuchsia::images::ImageInfo info;
};

// Asks for a screenshot of the display's current contents and returns it.
//
// fuchsia.ui.composition.Screenshot is expected to be in |services|.
::fpromise::promise<ScreenshotData, Error> TakeScreenshot(
    async_dispatcher_t* dispatcher, std::shared_ptr<sys::ServiceDirectory> services,
    zx::duration timeout);

}  // namespace forensics::feedback_data

#endif  // SRC_DEVELOPER_FORENSICS_FEEDBACK_DATA_SCREENSHOT_H_

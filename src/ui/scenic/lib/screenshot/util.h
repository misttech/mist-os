// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_SCREENSHOT_UTIL_H_
#define SRC_UI_SCENIC_LIB_SCREENSHOT_UTIL_H_

#include <fuchsia/ui/composition/cpp/fidl.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/default.h>
#include <lib/async/dispatcher.h>
#include <lib/vfs/cpp/vmo_file.h>

namespace screenshot {

// Serves a screenshot through a channel using |fuchsia.io.File|
// Updates an unordered_map of current screenshots being served.
bool ServeScreenshot(
    zx::channel channel, zx::vmo response_vmo, size_t screenshot_index,
    std::unordered_map<size_t,
                       std::pair<std::unique_ptr<vfs::VmoFile>, std::unique_ptr<async::WaitOnce>>>*
        served_screenshots_);

}  // namespace screenshot

#endif  // SRC_UI_SCENIC_LIB_SCREENSHOT_UTIL_H_

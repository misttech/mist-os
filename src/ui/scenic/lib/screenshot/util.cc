// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screenshot/util.h"

#include <lib/syslog/cpp/macros.h>

namespace screenshot {
bool ServeScreenshot(
    zx::channel channel, zx::vmo response_vmo, size_t screenshot_index,
    std::unordered_map<size_t,
                       std::pair<std::unique_ptr<vfs::VmoFile>, std::unique_ptr<async::WaitOnce>>>*
        served_screenshots) {
  size_t vmo_size;
  zx_status_t zx_status = response_vmo.get_size(&vmo_size);
  if (zx_status != ZX_OK) {
    FX_PLOGS(ERROR, zx_status) << "Unable to get VMO size";
    return false;
  }

  auto served_screenshot = std::make_unique<vfs::VmoFile>(std::move(response_vmo), vmo_size);
  std::unique_ptr<async::WaitOnce> channel_closed_observer =
      std::make_unique<async::WaitOnce>(channel.get(), ZX_CHANNEL_PEER_CLOSED);

  served_screenshots->emplace(screenshot_index, std::make_pair(std::move(served_screenshot),
                                                               std::move(channel_closed_observer)));

  std::function<void()> completed = [served_screenshots, screenshot_index]() mutable {
    served_screenshots->erase(screenshot_index);
  };

  if (const auto status =
          served_screenshots->at(screenshot_index)
              .second->Begin(async_get_default_dispatcher(),
                             [completed = std::move(completed)](...) { completed(); });
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Cannot attach observer to server end";
    return false;
  }

  if (const auto status =
          served_screenshots->at(screenshot_index)
              .first->Serve(fuchsia::io::OpenFlags::RIGHT_READABLE, std::move(channel));
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Cannot serve screenshot";
    return false;
  }

  return true;
}
}  // namespace screenshot

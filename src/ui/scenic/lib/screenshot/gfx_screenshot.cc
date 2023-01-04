// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screenshot/gfx_screenshot.h"

#include <lib/syslog/cpp/macros.h>

#include <optional>

#include "zircon/rights.h"
namespace screenshot {

using fuchsia::ui::composition::ScreenshotTakeFileResponse;
using fuchsia::ui::composition::ScreenshotTakeRequest;
using fuchsia::ui::composition::ScreenshotTakeResponse;

GfxScreenshot::GfxScreenshot(TakeGfxScreenshot take_gfx_screenshot,
                             fit::function<void(GfxScreenshot*)> destroy_instance_function)
    : take_gfx_screenshot_(std::move(take_gfx_screenshot)),
      destroy_instance_function_(std::move(destroy_instance_function)),
      weak_factory_(this) {}

GfxScreenshot::~GfxScreenshot() = default;

void GfxScreenshot::Take(fuchsia::ui::composition::ScreenshotTakeRequest params,
                         TakeCallback callback) {
  if (take_callback_ != nullptr) {
    FX_LOGS(ERROR) << "Screenshot::Take() already in progress, closing connection. Wait for return "
                      "before calling again.";
    destroy_instance_function_(this);
    return;
  }

  take_callback_ = std::move(callback);

  take_gfx_screenshot_([weak_ptr = weak_factory_.GetWeakPtr()](
                           fuchsia::ui::scenic::ScreenshotData data, bool success) {
    if (!weak_ptr) {
      return;
    }

    if (!success) {
      weak_ptr->destroy_instance_function_(weak_ptr.get());
      return;
    }

    fuchsia::ui::composition::ScreenshotTakeResponse response;

    zx::vmo response_vmo;
    zx_status_t status = data.data.vmo.duplicate(
        ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_TRANSFER | ZX_RIGHT_GET_PROPERTY, &response_vmo);
    FX_CHECK(status == ZX_OK);
    response.set_vmo(std::move(response_vmo));
    response.set_size({data.info.width, data.info.height});

    weak_ptr->take_callback_(std::move(response));
    weak_ptr->take_callback_ = nullptr;
  });
}

void GfxScreenshot::TakeFile(fuchsia::ui::composition::ScreenshotTakeFileRequest params,
                             TakeFileCallback callback) {
  if (take_file_callback_ != nullptr) {
    FX_LOGS(ERROR)
        << "Screenshot::TakeFile() already in progress, closing connection. Wait for return "
           "before calling again.";
    destroy_instance_function_(this);
    return;
  }
  take_file_callback_ = std::move(callback);

  take_gfx_screenshot_([this, weak_ptr = weak_factory_.GetWeakPtr()](
                           fuchsia::ui::scenic::ScreenshotData data, bool success) {
    if (!weak_ptr) {
      return;
    }

    if (!success) {
      weak_ptr->destroy_instance_function_(weak_ptr.get());
      return;
    }

    fuchsia::ui::composition::ScreenshotTakeFileResponse response;

    zx::vmo response_vmo;
    zx_status_t status = data.data.vmo.duplicate(
        ZX_RIGHT_READ | ZX_RIGHT_MAP | ZX_RIGHT_TRANSFER | ZX_RIGHT_GET_PROPERTY, &response_vmo);
    FX_CHECK(status == ZX_OK);

    response.set_size({data.info.width, data.info.height});

    fidl::InterfaceHandle<fuchsia::io::File> file_client;

    fidl::InterfaceRequest<fuchsia::io::File> file_server = file_client.NewRequest();

    if (!file_server.is_valid()) {
      FX_LOGS(ERROR) << "Cannot create file server channel";
      return;
    }

    const size_t screenshot_index = served_screenshots_next_id_++;
    if (!ServeScreenshot(file_server.TakeChannel(), std::move(response_vmo), screenshot_index,
                         &served_screenshots_)) {
      return;
    }

    response.set_file(std::move(file_client));

    weak_ptr->take_file_callback_(std::move(response));
    weak_ptr->take_file_callback_ = nullptr;
  });
}
}  // namespace screenshot

// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/screenshot/screenshot_manager.h"

#include <lib/syslog/cpp/macros.h>

#include <memory>

#include "lib/component/incoming/cpp/protocol.h"
#include "src/ui/scenic/lib/screen_capture/screen_capture.h"

namespace screenshot {

ScreenshotManager::ScreenshotManager(
    std::shared_ptr<allocation::Allocator> allocator, std::shared_ptr<flatland::Renderer> renderer,
    GetRenderables get_renderables,
    std::vector<std::shared_ptr<allocation::BufferCollectionImporter>> buffer_collection_importers,
    fuchsia::math::SizeU display_size, int display_rotation)
    : allocator_(std::move(allocator)),
      renderer_(renderer),
      get_renderables_(std::move(get_renderables)),
      buffer_collection_importers_(std::move(buffer_collection_importers)),
      display_size_(display_size),
      display_rotation_(display_rotation) {
  FX_DCHECK(renderer_);
}

void ScreenshotManager::CreateBinding(
    fidl::InterfaceRequest<fuchsia::ui::composition::Screenshot> request) {
  std::unique_ptr<ScreenCapture> screen_capture = std::make_unique<ScreenCapture>(
      buffer_collection_importers_, renderer_, [this]() { return get_renderables_(); });

  // fuchsia_ui_compression_internal::ImageCompressor client_ptr;
  zx::result client_end = component::Connect<fuchsia_ui_compression_internal::ImageCompressor>();
  if (!client_end.is_ok()) {
    FX_LOGS(ERROR) << "Error connecting to the |ImageCompressor| protocol: "
                   << client_end.status_string();
    return;
  }

  CompressorEventHandler event_handler;
  fidl::Client client(std::move(*client_end), async_get_default_dispatcher(), &event_handler);

  bindings_.AddBinding(std::make_unique<screenshot::FlatlandScreenshot>(
                           std::move(screen_capture), allocator_, display_size_, display_rotation_,
                           std::move(client),
                           [this](screenshot::FlatlandScreenshot* sc) {
                             bindings_.CloseBinding(sc, ZX_ERR_SHOULD_WAIT);
                           }),
                       std::move(request));
}

}  // namespace screenshot

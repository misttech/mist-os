// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_COORDINATOR_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_COORDINATOR_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl_test_base.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <atomic>

#include <gmock/gmock.h>

namespace flatland {

class MockDisplayCoordinator : public fuchsia::hardware::display::testing::Coordinator_TestBase {
 public:
  explicit MockDisplayCoordinator(zx::channel coordinator_channel, async_dispatcher_t* dispatcher)
      : binding_(this, std::move(coordinator_channel), dispatcher) {
    binding_.set_error_handler([this](zx_status_t status) {
      if (status != ZX_ERR_PEER_CLOSED) {
        FX_LOGS(ERROR) << "FIDL binding is closed due to error " << zx_status_get_string(status);
      }
      is_bound_.store(false, std::memory_order_relaxed);
    });
  }

  bool IsBound() const { return is_bound_.load(std::memory_order_relaxed); }

  // TODO(https://fxbug.dev/324689624): Do not use gMock to generate mocking
  // methods.

  MOCK_METHOD(void, ImportEvent, (zx::event, fuchsia::hardware::display::EventId));

  MOCK_METHOD(void, SetLayerColorConfig,
              (fuchsia::hardware::display::LayerId, fuchsia::images2::PixelFormat,
               std::vector<uint8_t>));

  MOCK_METHOD(void, SetLayerImage,
              (fuchsia::hardware::display::LayerId, fuchsia::hardware::display::ImageId,
               fuchsia::hardware::display::EventId, fuchsia::hardware::display::EventId));

  MOCK_METHOD(void, ApplyConfig, ());

  MOCK_METHOD(void, GetLatestAppliedConfigStamp, (GetLatestAppliedConfigStampCallback));

  MOCK_METHOD(void, CheckConfig, (bool, CheckConfigCallback));

  MOCK_METHOD(void, ImportBufferCollection,
              (fuchsia::hardware::display::BufferCollectionId,
               fidl::InterfaceHandle<class ::fuchsia::sysmem::BufferCollectionToken>,
               ImportBufferCollectionCallback));

  MOCK_METHOD(void, SetBufferCollectionConstraints,
              (fuchsia::hardware::display::BufferCollectionId,
               fuchsia::hardware::display::types::ImageBufferUsage,
               SetBufferCollectionConstraintsCallback));

  MOCK_METHOD(void, ReleaseBufferCollection, (fuchsia::hardware::display::BufferCollectionId));

  MOCK_METHOD(void, ImportImage,
              (fuchsia::hardware::display::types::ImageMetadata,
               fuchsia::hardware::display::BufferId, fuchsia::hardware::display::ImageId,
               ImportImageCallback));

  MOCK_METHOD(void, ReleaseImage, (fuchsia::hardware::display::ImageId));

  MOCK_METHOD(void, SetLayerPrimaryConfig,
              (fuchsia::hardware::display::LayerId,
               fuchsia::hardware::display::types::ImageMetadata));

  MOCK_METHOD(void, SetLayerPrimaryPosition,
              (fuchsia::hardware::display::LayerId, fuchsia::hardware::display::types::Transform,
               fuchsia::hardware::display::types::Frame, fuchsia::hardware::display::types::Frame));

  MOCK_METHOD(void, SetLayerPrimaryAlpha,
              (fuchsia::hardware::display::LayerId, fuchsia::hardware::display::types::AlphaMode,
               float));

  MOCK_METHOD(void, CreateLayer, (CreateLayerCallback));

  MOCK_METHOD(void, DestroyLayer, (fuchsia::hardware::display::LayerId));

  MOCK_METHOD(void, SetDisplayLayers,
              (fuchsia::hardware::display::types::DisplayId,
               ::std::vector<fuchsia::hardware::display::LayerId>));

  MOCK_METHOD(void, SetDisplayColorConversion,
              (fuchsia::hardware::display::types::DisplayId, (std::array<float, 3>),
               (std::array<float, 9>), (std::array<float, 3>)));

 private:
  void NotImplemented_(const std::string& name) final {}

  fidl::Binding<fuchsia::hardware::display::Coordinator> binding_;
  std::atomic<bool> is_bound_ = true;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_COORDINATOR_H_

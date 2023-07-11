// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_CONTROLLER_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_CONTROLLER_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl_test_base.h>

namespace flatland {

class MockDisplayCoordinator : public fuchsia::hardware::display::testing::Coordinator_TestBase {
 public:
  explicit MockDisplayCoordinator() : binding_(this) {}

  void WaitForMessage() { binding_.WaitForMessage(); }

  void Bind(zx::channel device_channel, zx::channel coordinator_channel,
            async_dispatcher_t* dispatcher = nullptr) {
    device_channel_ = std::move(device_channel);
    binding_.Bind(fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator>(
                      std::move(coordinator_channel)),
                  dispatcher);
  }

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
               fuchsia::hardware::display::ImageConfig, SetBufferCollectionConstraintsCallback));

  MOCK_METHOD(void, ReleaseBufferCollection, (fuchsia::hardware::display::BufferCollectionId));

  MOCK_METHOD(void, ImportImage,
              (fuchsia::hardware::display::ImageConfig, fuchsia::hardware::display::BufferId,
               fuchsia::hardware::display::ImageId, ImportImageCallback));

  MOCK_METHOD(void, ReleaseImage, (fuchsia::hardware::display::ImageId));

  MOCK_METHOD(void, SetLayerPrimaryConfig,
              (fuchsia::hardware::display::LayerId, fuchsia::hardware::display::ImageConfig));

  MOCK_METHOD(void, SetLayerPrimaryPosition,
              (fuchsia::hardware::display::LayerId, fuchsia::hardware::display::Transform,
               fuchsia::hardware::display::Frame, fuchsia::hardware::display::Frame));

  MOCK_METHOD(void, SetLayerPrimaryAlpha,
              (fuchsia::hardware::display::LayerId, fuchsia::hardware::display::AlphaMode, float));

  MOCK_METHOD(void, CreateLayer, (CreateLayerCallback));

  MOCK_METHOD(void, DestroyLayer, (fuchsia::hardware::display::LayerId));

  MOCK_METHOD(void, SetDisplayLayers,
              (fuchsia::hardware::display::DisplayId,
               ::std::vector<fuchsia::hardware::display::LayerId>));

  MOCK_METHOD(void, SetDisplayColorConversion,
              (fuchsia::hardware::display::DisplayId, (std::array<float, 3>),
               (std::array<float, 9>), (std::array<float, 3>)));

 private:
  void NotImplemented_(const std::string& name) final {}

  fidl::Binding<fuchsia::hardware::display::Coordinator> binding_;
  zx::channel device_channel_;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_CONTROLLER_H_

// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_COORDINATOR_H_
#define SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_COORDINATOR_H_

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/test_base.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <atomic>

#include <gmock/gmock.h>

namespace flatland {

class MockDisplayCoordinator
    : public fidl::testing::TestBase<fuchsia_hardware_display::Coordinator> {
 public:
  MockDisplayCoordinator() = default;

  // Must be called from the dispatcher thread.
  void Bind(fidl::ServerEnd<fuchsia_hardware_display::Coordinator> server_end,
            async_dispatcher_t* dispatcher) {
    is_bound_.store(true, std::memory_order_relaxed);
    binding_ = std::make_unique<fidl::ServerBinding<fuchsia_hardware_display::Coordinator>>(
        dispatcher, std::move(server_end), this, [this](fidl::UnbindInfo info) {
          if (!info.is_peer_closed()) {
            FX_LOGS(INFO) << "FIDL binding is closed: " << info.FormatDescription();
          }
          is_bound_.store(false, std::memory_order_relaxed);
        });
  }

  bool IsBound() const { return is_bound_.load(std::memory_order_relaxed); }

  // TODO(https://fxbug.dev/324689624): Do not use gMock to generate mocking
  // methods.

  MOCK_METHOD(void, ImportEvent, (ImportEventRequest&, ImportEventCompleter::Sync&), (override));

  MOCK_METHOD(void, SetLayerColorConfig,
              (SetLayerColorConfigRequest&, SetLayerColorConfigCompleter::Sync&), (override));

  MOCK_METHOD(void, SetLayerImage2, (SetLayerImage2Request&, SetLayerImage2Completer::Sync&),
              (override));

  MOCK_METHOD(void, ApplyConfig, (ApplyConfigCompleter::Sync&), (override));

  MOCK_METHOD(void, GetLatestAppliedConfigStamp, (GetLatestAppliedConfigStampCompleter::Sync&),
              (override));

  MOCK_METHOD(void, CheckConfig, (CheckConfigRequest&, CheckConfigCompleter::Sync&), (override));

  MOCK_METHOD(void, ImportBufferCollection,
              (ImportBufferCollectionRequest&, ImportBufferCollectionCompleter::Sync&), (override));

  MOCK_METHOD(void, SetBufferCollectionConstraints,
              (SetBufferCollectionConstraintsRequest&,
               SetBufferCollectionConstraintsCompleter::Sync&),
              (override));

  MOCK_METHOD(void, ReleaseBufferCollection,
              (ReleaseBufferCollectionRequest&, ReleaseBufferCollectionCompleter::Sync&),
              (override));

  MOCK_METHOD(void, ImportImage, (ImportImageRequest&, ImportImageCompleter::Sync&), (override));

  MOCK_METHOD(void, ReleaseImage, (ReleaseImageRequest&, ReleaseImageCompleter::Sync&), (override));

  MOCK_METHOD(void, SetLayerPrimaryConfig,
              (SetLayerPrimaryConfigRequest&, SetLayerPrimaryConfigCompleter::Sync&), (override));

  MOCK_METHOD(void, SetLayerPrimaryPosition,
              (SetLayerPrimaryPositionRequest&, SetLayerPrimaryPositionCompleter::Sync&),
              (override));

  MOCK_METHOD(void, SetLayerPrimaryAlpha,
              (SetLayerPrimaryAlphaRequest&, SetLayerPrimaryAlphaCompleter::Sync&), (override));

  MOCK_METHOD(void, CreateLayer, (CreateLayerCompleter::Sync&), (override));

  MOCK_METHOD(void, DestroyLayer, (DestroyLayerRequest&, DestroyLayerCompleter::Sync&), (override));

  MOCK_METHOD(void, SetDisplayLayers, (SetDisplayLayersRequest&, SetDisplayLayersCompleter::Sync&),
              (override));

  MOCK_METHOD(void, SetDisplayColorConversion,
              (SetDisplayColorConversionRequest&, SetDisplayColorConversionCompleter::Sync&),
              (override));

 private:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  std::unique_ptr<fidl::ServerBinding<fuchsia_hardware_display::Coordinator>> binding_;
  std::atomic<bool> is_bound_ = false;
};

}  // namespace flatland

#endif  // SRC_UI_SCENIC_LIB_FLATLAND_ENGINE_TESTS_MOCK_DISPLAY_COORDINATOR_H_

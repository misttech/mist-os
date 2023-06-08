// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/markers.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/fidl/cpp/wire/client_base.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <zircon/errors.h>

#include <array>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/display-id.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"
#include "src/lib/testing/predicates/status.h"

namespace display {

TEST(DisplayTest, NoOpTest) { EXPECT_OK(ZX_OK); }

TEST(DisplayTest, ClientVSyncOk) {
  constexpr ConfigStamp kControllerStampValue(1);
  constexpr ConfigStamp kClientStampValue(2);

  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  ASSERT_OK(endpoints.status_value());
  auto& [client_end, server_end] = endpoints.value();

  Controller controller(nullptr);
  ClientProxy clientproxy(&controller, false, 0, std::move(server_end));
  clientproxy.EnableVsync(true);
  fbl::AutoLock lock(controller.mtx());
  clientproxy.UpdateConfigStampMapping({
      .controller_stamp = kControllerStampValue,
      .client_stamp = kClientStampValue,
  });

  EXPECT_OK(clientproxy.OnDisplayVsync(kInvalidDisplayId, 0, kControllerStampValue));

  fidl::WireSyncClient client(std::move(client_end));

  class EventHandler
      : public fidl::testing::WireSyncEventHandlerTestBase<fuchsia_hardware_display::Coordinator> {
   public:
    explicit EventHandler(ConfigStamp expected_config_stamp)
        : expected_config_stamp_(expected_config_stamp) {}

    void OnVsync(fidl::WireEvent<fuchsia_hardware_display::Coordinator::OnVsync>* event) override {
      ConfigStamp applied_config_stamp = ToConfigStamp(event->applied_config_stamp);
      if (applied_config_stamp == expected_config_stamp_) {
        vsync_handled_ = true;
      }
    }

    void NotImplemented_(const std::string& name) override { FAIL() << "Unexpected " << name; }

    bool vsync_handled_ = false;
    ConfigStamp expected_config_stamp_ = kInvalidConfigStamp;
  };

  EventHandler event_handler(kClientStampValue);
  EXPECT_TRUE(client.HandleOneEvent(event_handler).ok());
  EXPECT_TRUE(event_handler.vsync_handled_);

  clientproxy.CloseTest();
}

TEST(DisplayTest, ClientVSynPeerClosed) {
  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  ASSERT_OK(endpoints.status_value());
  auto& [client_end, server_end] = endpoints.value();

  Controller controller(nullptr);
  ClientProxy clientproxy(&controller, false, 0, std::move(server_end));
  clientproxy.EnableVsync(true);
  fbl::AutoLock lock(controller.mtx());
  client_end.reset();
  EXPECT_OK(clientproxy.OnDisplayVsync(kInvalidDisplayId, 0, kInvalidConfigStamp));
  clientproxy.CloseTest();
}

TEST(DisplayTest, ClientVSyncNotSupported) {
  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  ASSERT_OK(endpoints.status_value());
  auto& [client_end, server_end] = endpoints.value();

  Controller controller(nullptr);
  ClientProxy clientproxy(&controller, false, 0, std::move(server_end));
  fbl::AutoLock lock(controller.mtx());
  EXPECT_STATUS(ZX_ERR_NOT_SUPPORTED,
                clientproxy.OnDisplayVsync(kInvalidDisplayId, 0, kInvalidConfigStamp));
  clientproxy.CloseTest();
}

TEST(DisplayTest, ClientMustDrainPendingStamps) {
  constexpr size_t kNumPendingStamps = 5;
  constexpr std::array<uint64_t, kNumPendingStamps> kControllerStampValues = {1u, 2u, 3u, 4u, 5u};
  constexpr std::array<uint64_t, kNumPendingStamps> kClientStampValues = {2u, 3u, 4u, 5u, 6u};

  zx::result<fidl::Endpoints<fuchsia_hardware_display::Coordinator>> endpoints =
      fidl::CreateEndpoints<fuchsia_hardware_display::Coordinator>();
  ASSERT_OK(endpoints.status_value());
  auto& [client_end, server_end] = endpoints.value();

  Controller controller(nullptr);
  ClientProxy clientproxy(&controller, false, 0, std::move(server_end));
  clientproxy.EnableVsync(false);
  fbl::AutoLock lock(controller.mtx());
  for (size_t i = 0; i < kNumPendingStamps; i++) {
    clientproxy.UpdateConfigStampMapping({
        .controller_stamp = ConfigStamp(kControllerStampValues[i]),
        .client_stamp = ConfigStamp(kClientStampValues[i]),
    });
  }

  EXPECT_STATUS(
      ZX_ERR_NOT_SUPPORTED,
      clientproxy.OnDisplayVsync(kInvalidDisplayId, 0, ConfigStamp(kControllerStampValues.back())));

  // Even if Vsync is disabled, ClientProxy should always drain pending
  // controller stamps.
  EXPECT_EQ(clientproxy.pending_applied_config_stamps().size(), 1u);
  EXPECT_EQ(clientproxy.pending_applied_config_stamps().front().controller_stamp,
            ConfigStamp(kControllerStampValues.back()));

  clientproxy.CloseTest();
}

}  // namespace display

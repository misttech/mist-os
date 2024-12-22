// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.hardware.display/cpp/wire_test_base.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fidl/cpp/wire/client_base.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/errors.h>

#include <array>

#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/engine-driver-client.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

namespace {

struct DispatcherAndShutdownCompletion {
  fdf::SynchronizedDispatcher dispatcher;
  std::shared_ptr<libsync::Completion> dispatcher_shutdown_completion;
};

DispatcherAndShutdownCompletion CreateDispatcherAndShutdownCompletionForTesting() {
  auto shutdown_completion = std::make_shared<libsync::Completion>();
  zx::result<fdf::SynchronizedDispatcher> create_result = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "display-test-dispatcher",
      [shutdown_completion](fdf_dispatcher_t*) { shutdown_completion->Signal(); });
  ZX_ASSERT_MSG(create_result.is_ok(), "Failed to create dispatcher: %s",
                create_result.status_string());
  return {.dispatcher = std::move(create_result).value(),
          .dispatcher_shutdown_completion = std::move(shutdown_completion)};
}

class MockCoordinatorListener
    : public fidl::WireServer<fuchsia_hardware_display::CoordinatorListener> {
 public:
  MockCoordinatorListener() = default;
  ~MockCoordinatorListener() = default;

  void OnDisplaysChanged(OnDisplaysChangedRequestView request,
                         OnDisplaysChangedCompleter::Sync& completer) override {
    latest_added_display_infos_ = std::vector(request->added.begin(), request->added.end());
    latest_removed_display_ids_ = {};
    for (fuchsia_hardware_display_types::wire::DisplayId fidl_id : request->removed) {
      latest_removed_display_ids_.push_back(display::ToDisplayId(fidl_id));
    }
  }

  void OnVsync(OnVsyncRequestView request, OnVsyncCompleter::Sync& completer) override {
    latest_vsync_timestamp_ = zx::time(request->timestamp);
    latest_applied_config_stamp_ = display::ToConfigStamp(request->applied_config_stamp);
  }

  void OnClientOwnershipChange(OnClientOwnershipChangeRequestView request,
                               OnClientOwnershipChangeCompleter::Sync& completer) override {
    client_has_ownership_ = request->has_ownership;
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_display::CoordinatorListener> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  std::vector<fuchsia_hardware_display::wire::Info> latest_added_display_infos() const {
    return latest_added_display_infos_;
  }
  std::vector<display::DisplayId> latest_removed_display_ids() const {
    return latest_removed_display_ids_;
  }
  bool client_has_ownership() const { return client_has_ownership_; }
  zx::time latest_vsync_timestamp() const { return latest_vsync_timestamp_; }
  display::ConfigStamp latest_applied_config_stamp() const { return latest_applied_config_stamp_; }

 private:
  std::vector<fuchsia_hardware_display::wire::Info> latest_added_display_infos_;
  std::vector<display::DisplayId> latest_removed_display_ids_;
  bool client_has_ownership_ = false;
  zx::time latest_vsync_timestamp_ = zx::time::infinite_past();
  display::ConfigStamp latest_applied_config_stamp_ = display::kInvalidConfigStamp;
};

class CoordinatorClientWithListenerTest : public ::testing::Test {
 private:
  fdf_testing::ScopedGlobalLogger logger_;
};

TEST_F(CoordinatorClientWithListenerTest, ClientVSyncOk) {
  fdf_testing::DriverRuntime driver_runtime;

  constexpr display::ConfigStamp kControllerStampValue(1);
  constexpr display::ConfigStamp kClientStampValue(2);

  auto [coordinator_client_end, coordinator_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client_end, listener_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  auto [engine_client_end, engine_server_end] =
      fdf::Endpoints<fuchsia_hardware_display_engine::Engine>::Create();
  auto engine_driver_client = std::make_unique<EngineDriverClient>(std::move(engine_client_end));

  auto [dispatcher, shutdown_completion] = CreateDispatcherAndShutdownCompletionForTesting();
  Controller controller(std::move(engine_driver_client), dispatcher.borrow());

  ClientProxy clientproxy(&controller, ClientPriority::kPrimary, ClientId(1),
                          /*on_client_disconnected=*/[] {});
  ASSERT_OK(clientproxy.InitForTesting(std::move(coordinator_server_end),
                                       std::move(listener_client_end)));

  clientproxy.EnableVsync(true);
  fbl::AutoLock lock(controller.mtx());
  clientproxy.UpdateConfigStampMapping({
      .controller_stamp = kControllerStampValue,
      .client_stamp = kClientStampValue,
  });

  EXPECT_OK(clientproxy.OnDisplayVsync(display::kInvalidDisplayId, 0, kControllerStampValue));

  MockCoordinatorListener mock_coordinator_listener;
  fidl::ServerBindingRef<fuchsia_hardware_display::CoordinatorListener> listener_server_binding =
      fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                       std::move(listener_server_end), &mock_coordinator_listener);

  driver_runtime.RunUntilIdle();
  EXPECT_EQ(mock_coordinator_listener.latest_applied_config_stamp(), kClientStampValue);

  clientproxy.CloseForTesting();

  dispatcher.ShutdownAsync();
  shutdown_completion->Wait();
}

TEST_F(CoordinatorClientWithListenerTest, ClientVSyncPeerClosed) {
  fdf_testing::DriverRuntime driver_runtime;

  auto [coordinator_client_end, coordinator_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client_end, listener_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  auto [engine_client_end, engine_server_end] =
      fdf::Endpoints<fuchsia_hardware_display_engine::Engine>::Create();
  auto engine_driver_client = std::make_unique<EngineDriverClient>(std::move(engine_client_end));

  auto [dispatcher, shutdown_completion] = CreateDispatcherAndShutdownCompletionForTesting();
  Controller controller(std::move(engine_driver_client), dispatcher.borrow());

  ClientProxy clientproxy(&controller, ClientPriority::kPrimary, ClientId(1),
                          /*on_client_disconnected=*/[] {});
  ASSERT_OK(clientproxy.InitForTesting(std::move(coordinator_server_end),
                                       std::move(listener_client_end)));

  clientproxy.EnableVsync(true);
  fbl::AutoLock lock(controller.mtx());
  listener_client_end.reset();
  EXPECT_OK(
      clientproxy.OnDisplayVsync(display::kInvalidDisplayId, 0, display::kInvalidConfigStamp));
  clientproxy.CloseForTesting();

  dispatcher.ShutdownAsync();
  shutdown_completion->Wait();
}

TEST_F(CoordinatorClientWithListenerTest, ClientVSyncNotSupported) {
  fdf_testing::DriverRuntime driver_runtime;

  auto [coordinator_client_end, coordinator_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client_end, listener_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  auto [engine_client_end, engine_server_end] =
      fdf::Endpoints<fuchsia_hardware_display_engine::Engine>::Create();
  auto engine_driver_client = std::make_unique<EngineDriverClient>(std::move(engine_client_end));

  auto [dispatcher, shutdown_completion] = CreateDispatcherAndShutdownCompletionForTesting();
  Controller controller(std::move(engine_driver_client), dispatcher.borrow());

  ClientProxy clientproxy(&controller, ClientPriority::kPrimary, ClientId(1),
                          /*on_client_disconnected=*/[] {});
  ASSERT_OK(clientproxy.InitForTesting(std::move(coordinator_server_end),
                                       std::move(listener_client_end)));

  fbl::AutoLock lock(controller.mtx());
  EXPECT_STATUS(ZX_ERR_NOT_SUPPORTED, clientproxy.OnDisplayVsync(display::kInvalidDisplayId, 0,
                                                                 display::kInvalidConfigStamp));
  clientproxy.CloseForTesting();

  dispatcher.ShutdownAsync();
  shutdown_completion->Wait();
}

TEST_F(CoordinatorClientWithListenerTest, ClientMustDrainPendingStamps) {
  fdf_testing::DriverRuntime driver_runtime;

  constexpr size_t kNumPendingStamps = 5;
  constexpr std::array<uint64_t, kNumPendingStamps> kControllerStampValues = {1u, 2u, 3u, 4u, 5u};
  constexpr std::array<uint64_t, kNumPendingStamps> kClientStampValues = {2u, 3u, 4u, 5u, 6u};

  auto [coordinator_client_end, coordinator_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::Coordinator>::Create();
  auto [listener_client_end, listener_server_end] =
      fidl::Endpoints<fuchsia_hardware_display::CoordinatorListener>::Create();

  auto [engine_client_end, engine_server_end] =
      fdf::Endpoints<fuchsia_hardware_display_engine::Engine>::Create();
  auto engine_driver_client = std::make_unique<EngineDriverClient>(std::move(engine_client_end));

  auto [dispatcher, shutdown_completion] = CreateDispatcherAndShutdownCompletionForTesting();
  Controller controller(std::move(engine_driver_client), dispatcher.borrow());

  ClientProxy clientproxy(&controller, ClientPriority::kPrimary, ClientId(1),
                          /*on_client_disconnected=*/[] {});
  ASSERT_OK(clientproxy.InitForTesting(std::move(coordinator_server_end),
                                       std::move(listener_client_end)));

  clientproxy.EnableVsync(false);
  fbl::AutoLock lock(controller.mtx());
  for (size_t i = 0; i < kNumPendingStamps; i++) {
    clientproxy.UpdateConfigStampMapping({
        .controller_stamp = display::ConfigStamp(kControllerStampValues[i]),
        .client_stamp = display::ConfigStamp(kClientStampValues[i]),
    });
  }

  EXPECT_STATUS(ZX_ERR_NOT_SUPPORTED,
                clientproxy.OnDisplayVsync(display::kInvalidDisplayId, 0,
                                           display::ConfigStamp(kControllerStampValues.back())));

  // Even if Vsync is disabled, ClientProxy should always drain pending
  // controller stamps.
  EXPECT_EQ(clientproxy.pending_applied_config_stamps().size(), 1u);
  EXPECT_EQ(clientproxy.pending_applied_config_stamps().front().controller_stamp,
            display::ConfigStamp(kControllerStampValues.back()));

  clientproxy.CloseForTesting();

  dispatcher.ShutdownAsync();
  shutdown_completion->Wait();
}

}  // namespace

}  // namespace display_coordinator

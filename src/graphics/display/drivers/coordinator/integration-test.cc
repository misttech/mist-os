// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-testing/test_loop.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/fidl/cpp/wire/array.h>
#include <lib/zx/clock.h>
#include <lib/zx/time.h>
#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/controller.h"
#include "src/graphics/display/drivers/coordinator/testing/base.h"
#include "src/graphics/display/drivers/coordinator/testing/fidl_client.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/lib/api-types/cpp/buffer-collection-id.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/event-id.h"
#include "src/graphics/display/lib/api-types/cpp/image-id.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"
#include "src/lib/fsl/handles/object_info.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

class IntegrationTest : public TestBase {
 public:
  // Configures the VSync event delivery check in `IsClientActive()`.
  enum class VsyncCheck : bool {
    // The client may or may not be receiving VSync events.
    kNoCheck = false,

    // The client is receiving VSync events.
    kVsyncEnabled = true,
  };

  // Returns -1 if no display exists with the given ID.
  int64_t DisplayLayerCount(display::DisplayId id) {
    fbl::AutoLock<fbl::Mutex> controller_lock(CoordinatorController()->mtx());
    auto displays_it = CoordinatorController()->displays_.find(id);
    if (!displays_it.IsValid()) {
      return -1;
    }
    return int64_t{displays_it->layer_count};
  }

  // Returns null if there is no client connected at `client_priority`.
  static ClientProxy* GetClientProxy(Controller& coordinator_controller,
                                     ClientPriority client_priority)
      __TA_REQUIRES(coordinator_controller.mtx()) {
    switch (client_priority) {
      case ClientPriority::kPrimary:
        return coordinator_controller.primary_client_;
      case ClientPriority::kVirtcon:
        return coordinator_controller.virtcon_client_;
    }
    ZX_DEBUG_ASSERT_MSG(false, "Unimplemtened client priority: %d",
                        static_cast<int>(client_priority));
    return nullptr;
  }

  // True iff `client_priority` is the display coordinator's active client
  // and the client's Vsync delivery configuration matches `vsync_check`.
  bool IsClientActive(ClientPriority client_priority,
                      VsyncCheck vsync_check = VsyncCheck::kVsyncEnabled) {
    Controller& coordinator_controller = *CoordinatorController();
    fbl::AutoLock<fbl::Mutex> controller_lock(coordinator_controller.mtx());
    ClientProxy* client_proxy = GetClientProxy(coordinator_controller, client_priority);
    if (client_proxy == nullptr) {
      return false;
    }
    if (coordinator_controller.active_client_ != client_proxy) {
      return false;
    }

    if (vsync_check == VsyncCheck::kVsyncEnabled) {
      fbl::AutoLock<fbl::Mutex> client_proxy_lock(&client_proxy->mtx_);
      if (!client_proxy->vsync_delivery_enabled_) {
        return false;
      }
    }

    return true;
  }

  display::VsyncAckCookie MostRecentAckedCookie(ClientPriority client_priority) {
    Controller& coordinator_controller = *CoordinatorController();
    fbl::AutoLock<fbl::Mutex> controller_lock(coordinator_controller.mtx());
    ClientProxy* client_proxy = GetClientProxy(coordinator_controller, client_priority);
    ZX_ASSERT(client_proxy != nullptr);

    fbl::AutoLock<fbl::Mutex> client_proxy_lock(&client_proxy->mtx_);
    return client_proxy->handler_.LatestAckedCookie();
  }

  void SendVsyncAfterUnbind(std::unique_ptr<TestFidlClient> client, display::DisplayId display_id) {
    fbl::AutoLock<fbl::Mutex> controller_lock(CoordinatorController()->mtx());
    // Resetting the client will *start* client tear down.
    //
    // ~MockCoordinatorListener fences the server-side dispatcher thread (consistent with the
    // threading model of its fidl server binding), but that doesn't sync with the client end
    // (intentionally).
    client.reset();
    ClientProxy* client_ptr = CoordinatorController()->active_client_;
    EXPECT_OK(sync_completion_wait(client_ptr->handler_.fidl_unbound(), zx::sec(1).get()));
    // SetVsyncEventDelivery(false) has not completed here, because we are still
    // holding controller()->mtx()
    client_ptr->OnDisplayVsync(display_id, 0, display::kInvalidConfigStamp);
  }

  bool IsClientConnected(ClientPriority client_priority) {
    Controller& coordinator_controller = *CoordinatorController();
    fbl::AutoLock<fbl::Mutex> controller_lock(coordinator_controller.mtx());
    return GetClientProxy(coordinator_controller, client_priority) != nullptr;
  }

  void SendVsyncFromCoordinatorClientProxy() {
    fbl::AutoLock<fbl::Mutex> controller_lock(CoordinatorController()->mtx());
    CoordinatorController()->active_client_->OnDisplayVsync(display::kInvalidDisplayId, 0,
                                                            display::kInvalidConfigStamp);
  }

  void SendVsyncFromDisplayEngine() { FakeDisplayEngine().SendVsync(); }

  std::unique_ptr<TestFidlClient> OpenCoordinatorTestFidlClient(
      const fidl::WireSyncClient<fuchsia_sysmem2::Allocator>& sysmem_client,
      const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& display_provider_client,
      ClientPriority client_priority) {
    auto coordinator_client = std::make_unique<TestFidlClient>(sysmem_client_);
    zx::result<> open_coordinator_result =
        coordinator_client->OpenCoordinator(display_provider_client, client_priority, dispatcher());
    ZX_ASSERT_MSG(open_coordinator_result.is_ok(), "Failed to open coordinator: %s",
                  open_coordinator_result.status_string());

    bool poll_result =
        PollUntilOnLoop([&]() { return coordinator_client->HasOwnershipAndValidDisplay(); });
    ZX_ASSERT_MSG(poll_result,
                  "Failed to wait until client has ownership of the coordinator "
                  "and has a valid display");

    zx::result<> enable_vsync_result = coordinator_client->EnableVsyncEventDelivery();
    ZX_ASSERT_MSG(enable_vsync_result.is_ok(), "Failed to enable Vsync delivery for client: %s",
                  enable_vsync_result.status_string());

    return coordinator_client;
  }

  // |TestBase|
  void SetUp() override {
    TestBase::SetUp();
    auto sysmem = fidl::SyncClient(ConnectToSysmemAllocatorV2());
    EXPECT_TRUE(sysmem.is_valid());
    fuchsia_sysmem2::AllocatorSetDebugClientInfoRequest request;
    request.name() = fsl::GetCurrentProcessName();
    request.id() = fsl::GetCurrentProcessKoid();
    auto set_debug_result = sysmem->SetDebugClientInfo(std::move(request));
    EXPECT_TRUE(set_debug_result.is_ok());
    sysmem_client_ = fidl::WireSyncClient<fuchsia_sysmem2::Allocator>(sysmem.TakeClientEnd());
  }

  // |TestBase|
  void TearDown() override {
    // Wait until the display core has processed all client disconnections before sending the last
    // vsync.
    EXPECT_TRUE(PollUntilOnLoop([&]() { return !IsClientConnected(ClientPriority::kPrimary); }));
    EXPECT_TRUE(PollUntilOnLoop([&]() { return !IsClientConnected(ClientPriority::kVirtcon); }));

    // Send one last vsync, to make sure any blank configs take effect.
    SendVsyncFromDisplayEngine();
    EXPECT_EQ(0u, CoordinatorController()->ImportedImagesCountForTesting());
    TestBase::TearDown();
  }

 protected:
  fidl::WireSyncClient<fuchsia_sysmem2::Allocator> sysmem_client_;
};

TEST_F(IntegrationTest, DISABLED_ClientsCanBail) {
  for (size_t i = 0; i < 100; i++) {
    ASSERT_TRUE(PollUntilOnLoop([&]() { return !IsClientActive(ClientPriority::kPrimary); }));

    std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
        sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  }
}

TEST_F(IntegrationTest, MustUseUniqueEventIDs) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  zx::event event_a, event_b, event_c;
  ASSERT_OK(zx::event::create(0, &event_a));
  ASSERT_OK(zx::event::create(0, &event_b));
  ASSERT_OK(zx::event::create(0, &event_c));
  {
    fbl::AutoLock lock(client->mtx());
    static constexpr display::EventId kEventId(123);
    EXPECT_OK(client->dc_->ImportEvent(std::move(event_a), ToFidlEventId(kEventId)).status());
    // ImportEvent is one way. Expect the next call to fail.
    EXPECT_OK(client->dc_->ImportEvent(std::move(event_b), ToFidlEventId(kEventId)).status());
    // This test passes if it closes without deadlocking.
  }
  // TODO: Use LLCPP epitaphs when available to detect ZX_ERR_PEER_CLOSED.
}

TEST_F(IntegrationTest, SendVsyncsAfterEmptyConfig) {
  TestFidlClient virtcon_client(sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    fbl::AutoLock lock(virtcon_client.mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        virtcon_client.dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(virtcon_client.dc_->ApplyConfig().status());
  }

  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // Present an image
  EXPECT_OK(primary_client->PresentLayers());
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return DisplayLayerCount(primary_client->display_id()) == 1; }));
  uint64_t count = primary_client->vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->vsync_count() > count; }));

  // Set an empty config
  {
    fbl::AutoLock lock(primary_client->mtx());
    EXPECT_OK(
        primary_client->dc_->SetDisplayLayers(ToFidlDisplayId(primary_client->display_id()), {})
            .status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  display::ConfigStamp empty_config_stamp = CoordinatorController()->TEST_controller_stamp();
  // Wait for it to apply
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return DisplayLayerCount(primary_client->display_id()) == 0; }));

  // The old client disconnects
  primary_client.reset();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return !IsClientConnected(ClientPriority::kPrimary); }));

  // A new client connects
  primary_client = OpenCoordinatorTestFidlClient(sysmem_client_, DisplayProviderClient(),
                                                 ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  // ... and presents before the previous client's empty vsync
  EXPECT_OK(primary_client->PresentLayers());
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return DisplayLayerCount(primary_client->display_id()) == 1; }));

  // Empty vsync for last client. Nothing should be sent to the new client.
  const config_stamp_t banjo_config_stamp = ToBanjoConfigStamp(empty_config_stamp);
  CoordinatorController()->DisplayEngineListenerOnDisplayVsync(
      ToBanjoDisplayId(primary_client->display_id()), 0u, &banjo_config_stamp);

  // Send a second vsync, using the config the client applied.
  count = primary_client->vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->vsync_count() > count; }));
}

TEST_F(IntegrationTest, DISABLED_SendVsyncsAfterClientsBail) {
  TestFidlClient virtcon_client(sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    fbl::AutoLock lock(virtcon_client.mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        virtcon_client.dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(virtcon_client.dc_->ApplyConfig().status());
  }

  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // Present an image
  EXPECT_OK(primary_client->PresentLayers());
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return DisplayLayerCount(primary_client->display_id()) == 1; }));

  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->vsync_count() == 1; }));
  // Send the controller a vsync for an image / a config it won't recognize anymore.
  display::ConfigStamp invalid_config_stamp =
      CoordinatorController()->TEST_controller_stamp() - display::ConfigStamp{1};
  const config_stamp_t invalid_banjo_config_stamp = ToBanjoConfigStamp(invalid_config_stamp);
  CoordinatorController()->DisplayEngineListenerOnDisplayVsync(
      ToBanjoDisplayId(primary_client->display_id()), 0u, &invalid_banjo_config_stamp);

  // Send a second vsync, using the config the client applied.
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->vsync_count() == 2; }));
  EXPECT_EQ(2u, primary_client->vsync_count());
}

TEST_F(IntegrationTest, SendVsyncsAfterClientDies) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  auto id = primary_client->display_id();
  SendVsyncAfterUnbind(std::move(primary_client), id);
}

TEST_F(IntegrationTest, AcknowledgeVsync) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  EXPECT_EQ(0u, primary_client->vsync_count());

  // send vsyncs up to watermark level
  for (uint32_t i = 0; i < ClientProxy::kVsyncMessagesWatermark; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return primary_client->vsync_ack_cookie() != display::kInvalidVsyncAckCookie; }));
  EXPECT_EQ(ClientProxy::kVsyncMessagesWatermark, primary_client->vsync_count());

  // acknowledge
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(
        ToFidlVsyncAckCookieValue(primary_client->vsync_ack_cookie()));
  }
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
  }));
}

TEST_F(IntegrationTest, AcknowledgeVsyncAfterQueueFull) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsync
  uint32_t vsync_count = ClientProxy::kMaxVsyncMessages;
  while (vsync_count--) {
    SendVsyncFromCoordinatorClientProxy();
  }
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return (primary_client->vsync_count() == expected_vsync_count); }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(
        ToFidlVsyncAckCookieValue(primary_client->vsync_ack_cookie()));
  }
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages + kNumVsync + 1;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() == expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }
}

TEST_F(IntegrationTest, AcknowledgeVsyncAfterLongTime) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsyncs
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return primary_client->vsync_count() == ClientProxy::kMaxVsyncMessages; }));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a lot
  constexpr uint32_t kNumVsync = ClientProxy::kVsyncBufferSize * 10;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(
        ToFidlVsyncAckCookieValue(primary_client->vsync_ack_cookie()));
  }
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count =
        ClientProxy::kMaxVsyncMessages + ClientProxy::kVsyncBufferSize + 1;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() == expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }
}

TEST_F(IntegrationTest, InvalidVSyncCookie) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return (primary_client->vsync_count() == ClientProxy::kMaxVsyncMessages); }));
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync with invalid cookie
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(0xdeadbeef);
  }

  // This check can have a false positive pass, due to using a hard-coded
  // timeout.
  {
    zx::time deadline = zx::deadline_after(zx::sec(1));
    PollUntilOnLoop([&]() {
      if (zx::clock::get_monotonic() >= deadline)
        return true;
      return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
    });
  }
  EXPECT_NE(MostRecentAckedCookie(ClientPriority::kPrimary), primary_client->vsync_ack_cookie());

  // We should still not receive vsync events since acknowledge did not use valid cookie
  SendVsyncFromCoordinatorClientProxy();
  constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages;

  // This check can have a false positive pass, due to using a hard-coded
  // timeout.
  {
    zx::time deadline = zx::deadline_after(zx::sec(1));
    PollUntilOnLoop([&]() {
      if (zx::clock::get_monotonic() >= deadline)
        return true;
      return primary_client->vsync_count() == expected_vsync_count + 1;
    });
  }
  EXPECT_LT(primary_client->vsync_count(), expected_vsync_count + 1);

  EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
}

TEST_F(IntegrationTest, AcknowledgeVsyncWithOldCookie) {
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() == expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  constexpr uint32_t kNumVsync = 5;
  for (uint32_t i = 0; i < kNumVsync; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages, primary_client->vsync_count());

  // now let's acknowledge vsync
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(
        ToFidlVsyncAckCookieValue(primary_client->vsync_ack_cookie()));
  }
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages + kNumVsync + 1;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return (primary_client->vsync_count() == expected_vsync_count); }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }

  // save old cookie
  display::VsyncAckCookie old_vsync_ack_cookie = primary_client->vsync_ack_cookie();

  // send vsyncs until max vsync
  for (uint32_t i = 0; i < ClientProxy::kMaxVsyncMessages; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }

  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages * 2;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return (primary_client->vsync_count() == expected_vsync_count); }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }
  EXPECT_NE(display::kInvalidVsyncAckCookie, primary_client->vsync_ack_cookie());

  // At this point, display will not send any more vsync events. Let's confirm by sending a few
  for (uint32_t i = 0; i < ClientProxy::kVsyncBufferSize; i++) {
    SendVsyncFromCoordinatorClientProxy();
  }
  EXPECT_EQ(ClientProxy::kMaxVsyncMessages * 2, primary_client->vsync_count());

  // now let's acknowledge vsync with old cookie
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(ToFidlVsyncAckCookieValue(old_vsync_ack_cookie));
  }

  // This check can have a false positive pass, due to using a hard-coded
  // timeout.
  {
    zx::time deadline = zx::deadline_after(zx::sec(1));
    PollUntilOnLoop([&]() {
      if (zx::clock::get_monotonic() >= deadline)
        return true;
      return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
    });
  }
  EXPECT_NE(MostRecentAckedCookie(ClientPriority::kPrimary), primary_client->vsync_ack_cookie());

  // Since we did not acknowledge with most recent cookie, we should not get any vsync events back
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count = ClientProxy::kMaxVsyncMessages * 2;

    // This check can have a false positive pass, due to using a hard-coded
    // timeout.
    {
      zx::time deadline = zx::deadline_after(zx::sec(1));
      PollUntilOnLoop([&]() {
        if (zx::clock::get_monotonic() >= deadline)
          return true;
        return primary_client->vsync_count() == expected_vsync_count + 1;
      });
    }
    EXPECT_LT(primary_client->vsync_count(), expected_vsync_count + 1);

    // count should still remain the same
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }

  // now let's acknowledge with valid cookie
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->AcknowledgeVsync(
        ToFidlVsyncAckCookieValue(primary_client->vsync_ack_cookie()));
  }
  ASSERT_TRUE(PollUntilOnLoop([&]() {
    return MostRecentAckedCookie(ClientPriority::kPrimary) == primary_client->vsync_ack_cookie();
  }));

  // After acknowledge, we should expect to get all the stored messages + the latest vsync
  SendVsyncFromCoordinatorClientProxy();
  {
    static constexpr uint64_t expected_vsync_count =
        ClientProxy::kMaxVsyncMessages * 2 + ClientProxy::kVsyncBufferSize + 1;
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() == expected_vsync_count; }));
    EXPECT_EQ(expected_vsync_count, primary_client->vsync_count());
  }
}

TEST_F(IntegrationTest, CreateLayer) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  fbl::AutoLock lock(client->mtx());
  auto create_layer_reply = client->dc_->CreateLayer();
  ASSERT_OK(create_layer_reply.status());
  EXPECT_TRUE(create_layer_reply.value().is_ok());
}

TEST_F(IntegrationTest, ImportImageWithInvalidImageId) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  fbl::AutoLock lock(client->mtx());
  constexpr display::ImageId image_id = display::kInvalidImageId;
  constexpr display::BufferCollectionId buffer_collection_id(0xffeeeedd);
  fidl::WireResult<fuchsia_hardware_display::Coordinator::ImportImage> import_image_reply =
      client->dc_->ImportImage(
          client->displays_[0].image_metadata_,
          fuchsia_hardware_display::wire::BufferId{
              .buffer_collection_id = ToFidlBufferCollectionId(buffer_collection_id),
              .buffer_index = 0,
          },
          ToFidlImageId(image_id));
  ASSERT_OK(import_image_reply.status());
  EXPECT_TRUE(import_image_reply.value().is_error());
}

TEST_F(IntegrationTest, ImportImageWithNonExistentBufferCollectionId) {
  std::unique_ptr<TestFidlClient> client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  fbl::AutoLock lock(client->mtx());
  constexpr display::BufferCollectionId kNonExistentCollectionId(0xffeeeedd);
  constexpr display::ImageId image_id(1);
  fidl::WireResult<fuchsia_hardware_display::Coordinator::ImportImage> import_image_reply =
      client->dc_->ImportImage(
          client->displays_[0].image_metadata_,
          fuchsia_hardware_display::wire::BufferId{
              .buffer_collection_id = ToFidlBufferCollectionId(kNonExistentCollectionId),
              .buffer_index = 0,
          },
          ToFidlImageId(image_id));
  ASSERT_OK(import_image_reply.status());
  EXPECT_TRUE(import_image_reply.value().is_error());
}

TEST_F(IntegrationTest, ClampRgb) {
  TestFidlClient virtcon_client(sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    fbl::AutoLock lock(virtcon_client.mtx());
    // set mode to Fallback
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)virtcon_client.dc_->SetVirtconMode(fuchsia_hardware_display::VirtconMode::kFallback);
    ASSERT_TRUE(PollUntilOnLoop(
        [&]() { return IsClientActive(ClientPriority::kVirtcon, VsyncCheck::kNoCheck); }));
    // Clamp RGB to a minimum value
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)virtcon_client.dc_->SetMinimumRgb(32);
    ASSERT_TRUE(PollUntilOnLoop([&]() { return FakeDisplayEngine().GetClampRgbValue() == 32; }));
  }

  // Create a primary client
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));
  {
    fbl::AutoLock lock(primary_client->mtx());
    // Clamp RGB to a new value
    // TODO(https://fxbug.dev/42180237) Consider handling the error instead of ignoring it.
    (void)primary_client->dc_->SetMinimumRgb(1);
    ASSERT_TRUE(PollUntilOnLoop([&]() { return FakeDisplayEngine().GetClampRgbValue() == 1; }));
  }
  // close client and wait for virtcon to become active again
  primary_client.reset(nullptr);
  // Apply a config for virtcon client to become active.
  {
    fbl::AutoLock lock(virtcon_client.mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        virtcon_client.dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(virtcon_client.dc_->ApplyConfig().status());
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return IsClientActive(ClientPriority::kVirtcon, VsyncCheck::kNoCheck); }));
  SendVsyncFromDisplayEngine();
  // make sure clamp value was restored
  ASSERT_TRUE(PollUntilOnLoop([&]() { return FakeDisplayEngine().GetClampRgbValue() == 32; }));
}

// TODO(https://fxbug.dev/340926351): De-flake and reenable this test.
TEST_F(IntegrationTest, DISABLED_EmptyConfigIsNotApplied) {
  // Create and bind virtcon client.
  TestFidlClient virtcon_client(sysmem_client_);
  ASSERT_OK(virtcon_client.OpenCoordinator(DisplayProviderClient(), ClientPriority::kVirtcon,
                                           dispatcher()));
  {
    fbl::AutoLock lock(virtcon_client.mtx());
    EXPECT_OK(
        virtcon_client.dc_->SetVirtconMode(fuchsia_hardware_display::wire::VirtconMode::kFallback)
            .status());
  }
  {
    fbl::AutoLock lock(virtcon_client.mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        virtcon_client.dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(virtcon_client.dc_->ApplyConfig().status());
  }
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return IsClientActive(ClientPriority::kVirtcon, VsyncCheck::kNoCheck); }));

  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  // Virtcon client should remain active until primary client has set a config.
  uint64_t virtcon_client_vsync_count = virtcon_client.vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return virtcon_client.vsync_count() > virtcon_client_vsync_count; }));
  ASSERT_TRUE(PollUntilOnLoop([&]() { return primary_client->vsync_count() == 0; }));

  // Present an image from the primary client.
  EXPECT_OK(primary_client->PresentLayers());
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return DisplayLayerCount(primary_client->display_id()) == 1; }));

  // Primary client should have become active after a config was set.
  const uint64_t primary_vsync_count = primary_client->vsync_count();
  SendVsyncFromDisplayEngine();
  ASSERT_TRUE(
      PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
}

// This tests the basic behavior of ApplyConfig() and OnVsync() events.
// We test applying configurations with images without wait fences, so they are
// guaranteed to be ready when client calls ApplyConfig().
//
// In this case, the new configuration stamp is guaranteed to appear in the next
// coming OnVsync() event.
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_2
//  * ApplyConfig({}) ==> config_stamp_3
//  - Vsync now should have config_stamp_3
//
// Both images are ready at ApplyConfig() time, i.e. no fences are provided.
TEST_F(IntegrationTest, VsyncEvent) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  // Apply a config for client to become active.
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        primary_client->dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }

  auto present_config_stamp_0 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  zx::result<display::LayerId> create_default_layer_result = primary_client->CreateLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateImage();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();

  // Present one single image without wait.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_1 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_1, present_config_stamp_1);

  // Present another image layer without wait.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GT(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_2 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_2, present_config_stamp_2);

  // Hide the existing layer.
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        primary_client->dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  auto apply_config_stamp_3 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_3);
  EXPECT_GT(apply_config_stamp_3, apply_config_stamp_2);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(0, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_3 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_3, present_config_stamp_3);
}

// This tests the behavior of ApplyConfig() and OnVsync() events when images
// come with wait fences, which is a common use case in Scenic when using GPU
// composition.
//
// When applying configurations with pending images, the config_stamp returned
// from OnVsync() should not be updated unless the image becomes ready and
// triggers a ReapplyConfig().
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1, wait on fence1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_1
//  * Signal fence1
//  - Vsync now should have config_stamp_2
//
TEST_F(IntegrationTest, VsyncWaitForPendingImages) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  // Apply a config for client to become active.
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        primary_client->dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }

  auto present_config_stamp_0 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  zx::result<display::LayerId> create_default_layer_result = primary_client->CreateLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateImage();
  zx::result<TestFidlClient::EventInfo> create_image_1_ready_fence_result =
      primary_client->CreateEvent();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);
  EXPECT_OK(create_image_1_ready_fence_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();
  TestFidlClient::EventInfo image_1_ready_fence =
      std::move(create_image_1_ready_fence_result.value());

  // Present one single image without wait.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_1 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_1, present_config_stamp_1);

  // Present another image layer; but the image is not ready yet. So the
  // configuration applied to display device will be still the old one. On Vsync
  // the |presented_config_stamp| is still |config_stamp_1|.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = std::make_optional(image_1_ready_fence.id)},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GE(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_2 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_2, present_config_stamp_1);

  // Signal the event. Display Fence callback will be signaled, and new
  // configuration with new config stamp (config_stamp_2) will be used.
  // On next Vsync, the |presented_config_stamp| will be updated.
  auto old_controller_stamp = CoordinatorController()->TEST_controller_stamp();
  image_1_ready_fence.event.signal(0u, ZX_EVENT_SIGNALED);
  ASSERT_TRUE(PollUntilOnLoop([controller = CoordinatorController(), old_controller_stamp]() {
    return controller->TEST_controller_stamp() > old_controller_stamp;
  }));

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_3 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_3, apply_config_stamp_2);
}

// This tests the behavior of ApplyConfig() and OnVsync() events when images
// that comes with wait fences are hidden in subsequent configurations.
//
// If a pending image never becomes ready, the config_stamp returned from
// OnVsync() should not be updated unless the image layer has been removed from
// the display in a subsequent configuration.
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1, waiting on fence1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({}) ==> config_stamp_3
//  - Vsync now should have config_stamp_3
//
// Note that fence1 is never signaled.
//
TEST_F(IntegrationTest, VsyncHidePendingLayer) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);
  // Apply a config for client to become active.
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        primary_client->dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }

  auto present_config_stamp_0 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  zx::result<display::LayerId> create_default_layer_result = primary_client->CreateLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateImage();
  zx::result<TestFidlClient::EventInfo> create_image_1_ready_fence_result =
      primary_client->CreateEvent();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);
  EXPECT_OK(create_image_1_ready_fence_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();
  TestFidlClient::EventInfo image_1_ready_fence =
      std::move(create_image_1_ready_fence_result.value());

  // Present an image layer.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_1 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_1, present_config_stamp_1);

  // Present another image layer; but the image is not ready yet. Display
  // controller will wait on the fence and Vsync will return the previous
  // configuration instead.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = image_1_ready_fence.id},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GT(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_2 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_2, present_config_stamp_1);

  // Hide the image layer. Display controller will not care about the fence
  // and thus use the latest configuration stamp.
  {
    fbl::AutoLock lock(primary_client->mtx());
    // TODO(https://fxbug.dev/42080252): Do not hardcode the display ID, read from
    // display events instead.
    const display::DisplayId virtcon_display_id(1);
    EXPECT_OK(
        primary_client->dc_->SetDisplayLayers(ToFidlDisplayId(virtcon_display_id), {}).status());
    EXPECT_OK(primary_client->dc_->ApplyConfig().status());
  }
  auto apply_config_stamp_3 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_3);
  EXPECT_GE(apply_config_stamp_3, apply_config_stamp_2);

  // On Vsync, the configuration stamp client receives on Vsync event message
  // will be the latest one applied to the display controller, since the pending
  // image has been removed from the configuration.
  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(0, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_3 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_3, apply_config_stamp_3);
}

// This tests the behavior of ApplyConfig() and OnVsync() events when images
// that comes with wait fences are overridden in subsequent configurations.
//
// If a client applies a configuration (#1) with a pending image, while display
// controller waits for the image to be ready, the client may apply another
// configuration (#2) with a different image. If the image in configuration #2
// becomes available earlier than #1, the layer configuration in #1 should be
// overridden, and signaling wait fences in #1 should not trigger a
// ReapplyConfig().
//
// Here we test the following case:
//
//  * ApplyConfig({layerA: img0}) ==> config_stamp_1
//  - Vsync now should have config_stamp_1
//  * ApplyConfig({layerA: img1, waiting on fence1}) ==> config_stamp_2
//  - Vsync now should have config_stamp_1 since img1 is not ready yet
//  * ApplyConfig({layerA: img2, waiting on fence2}) ==> config_stamp_3
//  - Vsync now should have config_stamp_1 since img1 and img2 are not ready
//  * Signal fence2
//  - Vsync now should have config_stamp_3.
//  * Signal fence1
//  - Vsync .
//
// Note that fence1 is never signaled.
TEST_F(IntegrationTest, VsyncSkipOldPendingConfiguration) {
  // Create and bind primary client.
  std::unique_ptr<TestFidlClient> primary_client = OpenCoordinatorTestFidlClient(
      sysmem_client_, DisplayProviderClient(), ClientPriority::kPrimary);

  zx::result<display::LayerId> create_default_layer_result = primary_client->CreateLayer();
  zx::result<display::ImageId> create_image_0_result = primary_client->CreateImage();
  zx::result<display::ImageId> create_image_1_result = primary_client->CreateImage();
  zx::result<display::ImageId> create_image_2_result = primary_client->CreateImage();
  zx::result<TestFidlClient::EventInfo> create_image_1_ready_fence_result =
      primary_client->CreateEvent();
  zx::result<TestFidlClient::EventInfo> create_image_2_ready_fence_result =
      primary_client->CreateEvent();

  EXPECT_OK(create_default_layer_result);
  EXPECT_OK(create_image_0_result);
  EXPECT_OK(create_image_1_result);
  EXPECT_OK(create_image_2_result);
  EXPECT_OK(create_image_1_ready_fence_result);
  EXPECT_OK(create_image_2_ready_fence_result);

  display::LayerId default_layer_id = create_default_layer_result.value();
  display::ImageId image_0_id = create_image_0_result.value();
  display::ImageId image_1_id = create_image_1_result.value();
  display::ImageId image_2_id = create_image_2_result.value();
  TestFidlClient::EventInfo image_1_ready_fence =
      std::move(create_image_1_ready_fence_result.value());
  TestFidlClient::EventInfo image_2_ready_fence =
      std::move(create_image_2_ready_fence_result.value());

  // Apply a config for client to become active; Present an image layer.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_0_id,
       .image_ready_wait_event_id = std::nullopt},
  }));
  auto apply_config_stamp_0 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_0);
  ASSERT_TRUE(PollUntilOnLoop([&]() { return IsClientActive(ClientPriority::kPrimary); }));

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_0 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(apply_config_stamp_0, present_config_stamp_0);
  EXPECT_NE(0u, present_config_stamp_0.value());

  // Present another image layer (image #1, wait_event #0); but the image is not
  // ready yet. Display controller will wait on the fence and Vsync will return
  // the previous configuration instead.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_1_id,
       .image_ready_wait_event_id = image_1_ready_fence.id},
  }));
  auto apply_config_stamp_1 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_1);
  EXPECT_GT(apply_config_stamp_1, apply_config_stamp_0);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }

  auto present_config_stamp_1 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_1, present_config_stamp_0);

  // Present another image layer (image #2, wait_event #1); the image is not
  // ready as well. We should still see current |presented_config_stamp| to be
  // equal to |present_config_stamp_0|.
  EXPECT_OK(primary_client->PresentLayers({
      {.layer_id = default_layer_id,
       .image_id = image_2_id,
       .image_ready_wait_event_id = image_2_ready_fence.id},
  }));
  auto apply_config_stamp_2 = display::ToConfigStamp(primary_client->GetRecentAppliedConfigStamp());
  EXPECT_NE(display::kInvalidConfigStamp, apply_config_stamp_2);
  EXPECT_GT(apply_config_stamp_2, apply_config_stamp_1);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }

  auto present_config_stamp_2 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_2, present_config_stamp_1);

  // Signal the event #1. Display Fence callback will be signaled, and
  // configuration with new config stamp (apply_config_stamp_2) will be used.
  // On next Vsync, the |presented_config_stamp| will be updated.
  auto old_controller_stamp = CoordinatorController()->TEST_controller_stamp();
  image_2_ready_fence.event.signal(0u, ZX_EVENT_SIGNALED);
  ASSERT_TRUE(PollUntilOnLoop(
      [&]() { return CoordinatorController()->TEST_controller_stamp() > old_controller_stamp; }));

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_3 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_3, apply_config_stamp_2);

  // Signal the event #0. Since we have displayed a newer image, signaling the
  // old event associated with the old image shouldn't trigger ReapplyConfig().
  // We should still see |apply_config_stamp_2| as the latest presented config
  // stamp in the client.
  old_controller_stamp = CoordinatorController()->TEST_controller_stamp();
  image_1_ready_fence.event.signal(0u, ZX_EVENT_SIGNALED);

  {
    const uint64_t primary_vsync_count = primary_client->vsync_count();
    SendVsyncFromDisplayEngine();
    ASSERT_TRUE(
        PollUntilOnLoop([&]() { return primary_client->vsync_count() > primary_vsync_count; }));
  }
  EXPECT_EQ(1, DisplayLayerCount(primary_client->display_id()));

  auto present_config_stamp_4 = primary_client->recent_presented_config_stamp();
  EXPECT_EQ(present_config_stamp_4, apply_config_stamp_2);
}

// TODO(https://fxbug.dev/42171874): Currently the fake-display driver only supports one
// primary layer. In order to better test ApplyConfig() / OnVsync() behavior,
// we should make fake-display driver support multi-layer configurations and
// then we could add more multi-layer tests.

}  // namespace display_coordinator

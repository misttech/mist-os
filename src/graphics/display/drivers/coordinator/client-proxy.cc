// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/client-proxy.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/async/dispatcher.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/defer.h>
#include <lib/fit/function.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/sync/completion.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <algorithm>
#include <memory>
#include <span>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/string_printf.h>

#include "src/graphics/display/drivers/coordinator/client-id.h"
#include "src/graphics/display/drivers/coordinator/client-priority.h"
#include "src/graphics/display/drivers/coordinator/client.h"
#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/lib/api-types/cpp/config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/vsync-ack-cookie.h"
#include "src/graphics/display/lib/driver-utils/post-task.h"

namespace display_coordinator {

namespace {

// TODO(https://fxbug.dev/353627964): Make `AssertHeld()` a member function of `fbl::Mutex`.
void AssertHeld(fbl::Mutex& mutex) __TA_ASSERT(mutex) {
  ZX_DEBUG_ASSERT(mtx_trylock(mutex.GetInternal()) == thrd_busy);
}

}  // namespace

void ClientProxy::SetOwnership(bool is_owner) {
  fbl::AllocChecker ac;
  auto task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    fdf::warn("Failed to allocate set ownership task");
    return;
  }
  task->set_handler([this, client_handler = &handler_, is_owner](
                        async_dispatcher_t* /*dispatcher*/, async::Task* task, zx_status_t status) {
    if (status == ZX_OK && client_handler->IsValid()) {
      is_owner_property_.Set(is_owner);
      client_handler->SetOwnership(is_owner);
    }
    // Update `client_scheduled_tasks_`.
    fbl::AutoLock task_lock(&task_mtx_);
    auto it = std::find_if(client_scheduled_tasks_.begin(), client_scheduled_tasks_.end(),
                           [&](std::unique_ptr<async::Task>& t) { return t.get() == task; });
    // Current task must have been added to the list.
    ZX_DEBUG_ASSERT(it != client_scheduled_tasks_.end());
    client_scheduled_tasks_.erase(it);
  });
  fbl::AutoLock task_lock(&task_mtx_);
  if (task->Post(controller_.driver_dispatcher()->async_dispatcher()) == ZX_OK) {
    client_scheduled_tasks_.push_back(std::move(task));
  }
}

void ClientProxy::OnDisplaysChanged(std::span<const display::DisplayId> added_display_ids,
                                    std::span<const display::DisplayId> removed_display_ids) {
  handler_.OnDisplaysChanged(added_display_ids, removed_display_ids);
}

void ClientProxy::ReapplySpecialConfigs() {
  AssertHeld(*controller_.mtx());

  zx::result<> result = controller_.engine_driver_client()->SetMinimumRgb(handler_.GetMinimumRgb());
  if (!result.is_ok()) {
    fdf::error("Failed to reapply minimum RGB value: {}", result);
  }
}

void ClientProxy::ReapplyConfig() {
  fbl::AllocChecker ac;
  auto task = fbl::make_unique_checked<async::Task>(&ac);
  if (!ac.check()) {
    fdf::warn("Failed to reapply config");
    return;
  }

  task->set_handler([this, client_handler = &handler_](async_dispatcher_t* /*dispatcher*/,
                                                       async::Task* task, zx_status_t status) {
    if (status == ZX_OK && client_handler->IsValid()) {
      client_handler->ReapplyConfig();
    }
    // Update `client_scheduled_tasks_`.
    fbl::AutoLock task_lock(&task_mtx_);
    auto it = std::find_if(client_scheduled_tasks_.begin(), client_scheduled_tasks_.end(),
                           [&](std::unique_ptr<async::Task>& t) { return t.get() == task; });
    // Current task must have been added to the list.
    ZX_DEBUG_ASSERT(it != client_scheduled_tasks_.end());
    client_scheduled_tasks_.erase(it);
  });
  fbl::AutoLock task_lock(&task_mtx_);
  if (task->Post(controller_.driver_dispatcher()->async_dispatcher()) == ZX_OK) {
    client_scheduled_tasks_.push_back(std::move(task));
  }
}

void ClientProxy::OnCaptureComplete() {
  AssertHeld(*controller_.mtx());
  fbl::AutoLock l(&mtx_);
  if (enable_capture_) {
    handler_.CaptureCompleted();
  }
  enable_capture_ = false;
}

void ClientProxy::OnDisplayVsync(display::DisplayId display_id, zx_instant_mono_t timestamp,
                                 display::DriverConfigStamp driver_config_stamp) {
  AssertHeld(*controller_.mtx());

  display::ConfigStamp client_stamp = {};
  auto it =
      std::find_if(pending_applied_config_stamps_.begin(), pending_applied_config_stamps_.end(),
                   [driver_config_stamp](const ConfigStampPair& stamp) {
                     return stamp.driver_stamp >= driver_config_stamp;
                   });

  if (it == pending_applied_config_stamps_.end() || it->driver_stamp != driver_config_stamp) {
    client_stamp = display::kInvalidConfigStamp;
  } else {
    client_stamp = it->client_stamp;
    pending_applied_config_stamps_.erase(pending_applied_config_stamps_.begin(), it);
  }

  display::VsyncAckCookie vsync_ack_cookie = display::kInvalidVsyncAckCookie;
  if (number_of_vsyncs_sent_ >= (kVsyncMessagesWatermark - 1)) {
    // Number of  vsync events sent exceed the watermark level.
    // Check to see if client has been notified already that acknowledgement is needed.
    if (!acknowledge_request_sent_) {
      // We have not sent a (new) cookie to client for acknowledgement; do it now.
      // First, increment cookie sequence.
      ++vsync_cookie_sequence_;
      // Generate new cookie by xor'ing initial cookie with sequence number.
      vsync_ack_cookie = display::VsyncAckCookie(vsync_cookie_salt_ ^ vsync_cookie_sequence_);
    } else {
      // Client has already been notified; check if client has acknowledged it.
      ZX_DEBUG_ASSERT(last_cookie_sent_ != display::kInvalidVsyncAckCookie);
      if (handler_.LastAckedCookie() == last_cookie_sent_) {
        // Client has acknowledged cookie. Reset vsync tracking states
        number_of_vsyncs_sent_ = 0;
        acknowledge_request_sent_ = false;
        last_cookie_sent_ = display::kInvalidVsyncAckCookie;
      }
    }
  }

  if (number_of_vsyncs_sent_ >= kMaxVsyncMessages) {
    // We have reached/exceeded maximum allowed vsyncs without any acknowledgement. At this point,
    // start storing them.
    fdf::trace("Vsync not sent due to none acknowledgment.\n");
    ZX_DEBUG_ASSERT(vsync_ack_cookie == display::kInvalidVsyncAckCookie);
    if (buffered_vsync_messages_.full()) {
      buffered_vsync_messages_.pop();  // discard
    }
    buffered_vsync_messages_.push(VsyncMessageData{
        .display_id = display_id,
        .timestamp = timestamp,
        .config_stamp = client_stamp,
    });
    return;
  }

  // Send buffered vsync events before sending the latest.
  while (!buffered_vsync_messages_.empty()) {
    VsyncMessageData vsync_message_data = buffered_vsync_messages_.front();
    buffered_vsync_messages_.pop();
    handler_.NotifyVsync(vsync_message_data.display_id,
                         zx::time_monotonic{vsync_message_data.timestamp},
                         vsync_message_data.config_stamp, display::kInvalidVsyncAckCookie);
    number_of_vsyncs_sent_++;
  }

  // Send the latest vsync event.
  handler_.NotifyVsync(display_id, zx::time_monotonic{timestamp}, client_stamp, vsync_ack_cookie);

  // Update vsync tracking states.
  if (vsync_ack_cookie != display::kInvalidVsyncAckCookie) {
    acknowledge_request_sent_ = true;
    last_cookie_sent_ = vsync_ack_cookie;
  }
  number_of_vsyncs_sent_++;
}

void ClientProxy::OnClientDead() {
  ZX_DEBUG_ASSERT_MSG(on_client_disconnected_, "OnClientDead() called twice");

  // Stash any data members we need to access after the ClientProxy is deleted.
  fit::function<void()> on_client_disconnected = std::move(on_client_disconnected_);

  // Deletes `this`.
  controller_.OnClientDead(this);

  on_client_disconnected();
}

void ClientProxy::UpdateConfigStampMapping(ConfigStampPair stamps) {
  ZX_DEBUG_ASSERT(pending_applied_config_stamps_.empty() ||
                  pending_applied_config_stamps_.back().driver_stamp < stamps.driver_stamp);
  pending_applied_config_stamps_.push_back({
      .driver_stamp = stamps.driver_stamp,
      .client_stamp = stamps.client_stamp,
  });
}

display::VsyncAckCookie ClientProxy::LastVsyncAckCookieForTesting() {
  fbl::AutoLock<fbl::Mutex> lock(&mtx_);
  return handler_.LastAckedCookie();
}

sync_completion_t* ClientProxy::FidlUnboundCompletionForTesting() {
  fbl::AutoLock<fbl::Mutex> lock(&mtx_);
  return &fidl_unbound_completion_;
}

void ClientProxy::CloseForTesting() { handler_.TearDownForTesting(); }

void ClientProxy::TearDown() {
  ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());
  handler_.TearDown(ZX_ERR_CONNECTION_ABORTED);
}

zx_status_t ClientProxy::Init(
    inspect::Node* parent_node,
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
        coordinator_listener_client_end) {
  node_ =
      parent_node->CreateChild(fbl::StringPrintf("client-%" PRIu64, handler_.id().value()).c_str());
  node_.RecordString("priority", DebugStringFromClientPriority(handler_.priority()));
  is_owner_property_ = node_.CreateBool("is_owner", false);

  unsigned seed = static_cast<unsigned>(zx::clock::get_monotonic().get());
  vsync_cookie_salt_ = rand_r(&seed);

  fidl::OnUnboundFn<Client> unbound_callback =
      [this](Client* client, fidl::UnbindInfo info,
             fidl::ServerEnd<fuchsia_hardware_display::Coordinator> ch) {
        ZX_DEBUG_ASSERT(controller_.IsRunningOnDriverDispatcher());

        sync_completion_signal(&fidl_unbound_completion_);

        // Make sure we `TearDown()` so that no further tasks are scheduled on
        // the driver dispatcher.
        client->TearDown(ZX_OK);

        // The client has died. Notify the proxy, which will free the Client
        // instance.
        OnClientDead();
      };

  handler_.Bind(std::move(coordinator_server_end), std::move(coordinator_listener_client_end),
                std::move(unbound_callback));
  return ZX_OK;
}

zx::result<> ClientProxy::InitForTesting(
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server_end,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener>
        coordinator_listener_client_end) {
  // `ClientProxy` created by tests may not have a full-fledged display engine.
  // The production client teardown logic doesn't work here so we replace it with a no-op unbound
  // callback instead.
  fidl::OnUnboundFn<Client> unbound_callback =
      [](Client*, fidl::UnbindInfo, fidl::ServerEnd<fuchsia_hardware_display::Coordinator>) {};
  handler_.Bind(std::move(coordinator_server_end), std::move(coordinator_listener_client_end),
                std::move(unbound_callback));
  return zx::ok();
}

ClientProxy::ClientProxy(Controller* controller, ClientPriority client_priority, ClientId client_id,
                         fit::function<void()> on_client_disconnected)
    : controller_(*controller),
      handler_(&controller_, this, client_priority, client_id),
      on_client_disconnected_(std::move(on_client_disconnected)) {
  ZX_DEBUG_ASSERT(controller);
  ZX_DEBUG_ASSERT(on_client_disconnected_);
}

ClientProxy::~ClientProxy() {}

}  // namespace display_coordinator

// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_FAKE_COORDINATOR_CONNECTOR_SERVICE_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_FAKE_COORDINATOR_CONNECTOR_SERVICE_H_

#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/channel.h>

#include <memory>
#include <queue>

#include "src/graphics/display/drivers/fake/fake-display-stack.h"

namespace display {

// Connects clients to a fake display-coordinator device with a fake-display
// display engine.
//
// FakeDisplayCoordinatorConnector is not thread-safe. All public methods must
// be invoked on a single-threaded event loop with the same `dispatcher`
// provided on FakeDisplayCoordinatorConnector creation.
class FakeDisplayCoordinatorConnector : public fidl::Server<fuchsia_hardware_display::Provider> {
 public:
  // Creates a FakeDisplayCoordinatorConnector where the fake display driver
  // is initialized using `fake_display_device_config`, and then publish its
  // service to `component`'s outgoing service directory.
  // Callers must guarantee that all FIDL methods run on `dispatcher`.
  static zx::result<> CreateAndPublishService(
      std::shared_ptr<zx_device> mock_root, async_dispatcher_t* dispatcher,
      const fake_display::FakeDisplayDeviceConfig& fake_display_device_config,
      component::OutgoingDirectory& outgoing);

  // Creates a FakeDisplayCoordinatorConnector where the fake display driver
  // is initialized using `fake_display_device_config`.
  // Callers are responsible for binding incoming FIDL clients to it.
  // Callers must guarantee that all FIDL methods run on `dispatcher`.
  FakeDisplayCoordinatorConnector(
      std::shared_ptr<zx_device> mock_root, async_dispatcher_t* dispatcher,
      const fake_display::FakeDisplayDeviceConfig& fake_display_device_config);
  ~FakeDisplayCoordinatorConnector() override;

  // Disallow copy, assign and move.
  FakeDisplayCoordinatorConnector(const FakeDisplayCoordinatorConnector&) = delete;
  FakeDisplayCoordinatorConnector(FakeDisplayCoordinatorConnector&&) = delete;
  FakeDisplayCoordinatorConnector operator=(const FakeDisplayCoordinatorConnector&) = delete;
  FakeDisplayCoordinatorConnector operator=(FakeDisplayCoordinatorConnector&&) = delete;

  // `fidl::Server<fuchsia_hardware_display::Provider>`
  void OpenCoordinatorForVirtcon(OpenCoordinatorForVirtconRequest& request,
                                 OpenCoordinatorForVirtconCompleter::Sync& completer) override;
  void OpenCoordinatorForPrimary(OpenCoordinatorForPrimaryRequest& request,
                                 OpenCoordinatorForPrimaryCompleter::Sync& completer) override;

  // Check coordinator clients' connection status for tests only.
  int GetNumQueuedPrimaryRequestsTestOnly() const {
    return static_cast<int>(state_->queued_primary_requests.size());
  }

 private:
  struct OpenCoordinatorRequest {
    bool is_virtcon;
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_request;
    fit::function<void(zx_status_t)> on_coordinator_opened;
  };

  // Encapsulates state for thread safety, since |display::FakeDisplayStack| invokes callbacks
  // from other threads.
  // TODO(fxbug.dev/129571): The comments are vague since it lacks the thread-
  // safety model of the struct. We need to rigorize the thread-safety model and
  // make sure that the access pattern is correct.
  struct State {
    async_dispatcher_t* const dispatcher;

    const std::unique_ptr<display::FakeDisplayStack> fake_display_stack;

    bool primary_coordinator_claimed = false;
    bool virtcon_coordinator_claimed = false;
    std::queue<OpenCoordinatorRequest> queued_primary_requests;
    std::queue<OpenCoordinatorRequest> queued_virtcon_requests;

    bool IsCoordinatorClaimed(bool use_virtcon_coordinator) const {
      return use_virtcon_coordinator ? virtcon_coordinator_claimed : primary_coordinator_claimed;
    }
    // Claim the coordinator of the specified connection type, which must not
    // already be claimed.
    void MarkCoordinatorClaimed(bool use_virtcon_coordinator) {
      bool& claimed =
          use_virtcon_coordinator ? virtcon_coordinator_claimed : primary_coordinator_claimed;
      ZX_ASSERT_MSG(!claimed, "%s coordinator already claimed",
                    use_virtcon_coordinator ? "virtcon" : "primary");
      claimed = true;
    }
    // Unclaim the coordinator of the specified connection type, which must
    // already be claimed.
    void MarkCoordinatorUnclaimed(bool use_virtcon_coordinator) {
      bool& claimed =
          use_virtcon_coordinator ? virtcon_coordinator_claimed : primary_coordinator_claimed;
      ZX_ASSERT_MSG(claimed, "%s coordinator not claimed",
                    use_virtcon_coordinator ? "virtcon" : "primary");
      claimed = false;
    }
    std::queue<OpenCoordinatorRequest>& GetQueuedRequests(bool use_virtcon_coordinator) {
      return use_virtcon_coordinator ? queued_virtcon_requests : queued_primary_requests;
    }
  };

  // Connects `request` to the fake display coordinator if the coordinator is
  // available, or queue `request` and defer it until the coordinator is
  // available.
  //
  // Must be called from `state_->dispatcher` thread.
  void ConnectOrDeferClient(OpenCoordinatorRequest request);

  // Release the current coordinator and connects it to the next queued request
  // of the same request type specified by `use_virtcon_coordinator` if there
  // exists any queued request.
  //
  // The fake coordinator must be valid and claimed by the client about to
  // release the coordinator.
  // Must be called from `state->dispatcher` thread.
  static void ReleaseCoordinatorAndConnectToNextQueuedClient(bool use_virtcon_coordinator,
                                                             std::shared_ptr<State> state);

  // Connects `request` to the fake display coordinator.
  //
  // The fake coordinator must be valid and not yet claimed by other clients.
  // Must be called from `state->dispatcher` thread.
  static void ConnectClient(OpenCoordinatorRequest request, const std::shared_ptr<State>& state);

  std::shared_ptr<State> state_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_FAKE_COORDINATOR_CONNECTOR_SERVICE_H_

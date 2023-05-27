// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_FAKE_SERVICE_H_
#define SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_FAKE_SERVICE_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>

#include <memory>
#include <queue>

#include "src/graphics/display/drivers/fake/fake-display-stack.h"
#include "src/lib/fxl/macros.h"

namespace sys {
class ComponentContext;
}  // namespace sys

namespace fake_display {

// Not thread-safe.  The assumption is that the public methods will be invoked by FIDL bindings
// on a single-threaded event-loop.
class ProviderService : public fuchsia::hardware::display::Provider {
 public:
  // |app_context| is used to publish this service.
  ProviderService(std::shared_ptr<zx_device> parent, sys::ComponentContext* app_context,
                  async_dispatcher_t* dispatcher);
  ~ProviderService();

  // |fuchsia::hardware::display::Provider|.
  void OpenCoordinatorForVirtcon(
      ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator_request,
      OpenCoordinatorForVirtconCallback callback) override;

  // |fuchsia::hardware::display::Provider|.
  void OpenCoordinatorForPrimary(
      ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator_request,
      OpenCoordinatorForPrimaryCallback callback) override;

  // For tests.
  size_t num_queued_requests() const { return state_->queued_requests.size(); }
  size_t num_virtcon_queued_requests() const { return state_->virtcon_queued_requests.size(); }
  bool coordinator_claimed() const { return state_->coordinator_claimed; }
  bool virtcon_coordinator_claimed() const { return state_->virtcon_coordinator_claimed; }

 private:
  struct Request {
    bool is_virtcon;
    zx::channel device;
    ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> coordinator_request;
    OpenCoordinatorForPrimaryCallback callback;
  };

  // Encapsulates state for thread safety, since |display::FakeDisplayStack| invokes callbacks
  // from other threads.
  struct State {
    async_dispatcher_t* const dispatcher;

    std::unique_ptr<display::FakeDisplayStack> tree;

    bool coordinator_claimed = false;
    bool virtcon_coordinator_claimed = false;
    std::queue<Request> queued_requests;
    std::queue<Request> virtcon_queued_requests;
  };

  // Called by OpenVirtconCoordinator() and OpenCoordinator().
  void ConnectOrDeferClient(Request request);

  // Must be called from main dispatcher thread.
  static void ConnectClient(Request request, const std::shared_ptr<State>& state);

  std::shared_ptr<State> state_;

  fidl::BindingSet<fuchsia::hardware::display::Provider> bindings_;

  FXL_DISALLOW_COPY_ASSIGN_AND_MOVE(ProviderService);
};

}  // namespace fake_display

#endif  // SRC_GRAPHICS_DISPLAY_TESTING_COORDINATOR_PROVIDER_FAKE_SERVICE_H_

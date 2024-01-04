// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/process/lifecycle/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/sys/cpp/component_context.h>
#include <zircon/processargs.h>

#include "fuchsia/hardware/bluetooth/cpp/fidl.h"
#include "host_component.h"
#include "src/connectivity/bluetooth/core/bt-host/bt_host_config.h"
#include "src/connectivity/bluetooth/core/bt-host/common/log.h"
#include "util.h"

using InitCallback = fit::callback<void(bool success)>;
using ErrorCallback = fit::callback<void()>;

const std::string OUTGOING_SERVICE_NAME = "fuchsia.bluetooth.host.Host";

class LifecycleHandler : public fuchsia::process::lifecycle::Lifecycle {
 public:
  using WeakPtr = WeakSelf<bthost::BtHostComponent>::WeakPtr;

  explicit LifecycleHandler(async::Loop* loop, WeakPtr host) : loop_(loop), host_(std::move(host)) {
    // Get the PA_LIFECYCLE handle, and instantiate the channel with it
    zx::channel channel = zx::channel(zx_take_startup_handle(PA_LIFECYCLE));
    // Bind to the channel and start listening for events
    bindings_.AddBinding(
        this, fidl::InterfaceRequest<fuchsia::process::lifecycle::Lifecycle>(std::move(channel)),
        loop_->dispatcher());
  }

  void Stop() override {
    host_->ShutDown();
    loop_->Shutdown();
    bindings_.CloseAll();
  }

 private:
  async::Loop* loop_;
  WeakPtr host_;
  fidl::BindingSet<fuchsia::process::lifecycle::Lifecycle> bindings_;
};

int main() {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  bt_log(DEBUG, "bt-host", "Starting bt-host");

  bt_host_config::Config config = bt_host_config::Config::TakeFromStartupHandle();
  bt_log(DEBUG, "bt-host", "device_path: %s", config.device_path().c_str());

  std::unique_ptr<bthost::BtHostComponent> host =
      bthost::BtHostComponent::Create(loop.dispatcher(), config.device_path());

  LifecycleHandler lifecycle_handler(&loop, host->GetWeakPtr());

  auto init_cb = [&host, &lifecycle_handler](bool success) {
    BT_DEBUG_ASSERT(host);
    if (!success) {
      bt_log(ERROR, "bt-host", "Failed to initialize bt-host; shutting down...");
      // TODO(https://fxbug.dev/136534): Verify that calling Lifecycler handler's stop function does not
      // cause use after free in Adapter with integration tests
      lifecycle_handler.Stop();
    } else {
      bt_log(DEBUG, "bt-host", "bt-host initialized");
    }
  };
  auto error_cb = [&lifecycle_handler]() {
    bt_log(WARN, "bt-host", "Error initializing bt-host; shutting down...");
    // TODO(https://fxbug.dev/136534): Verify that calling Lifecycler handler's stop function does not cause
    // use after free in Adapter with integration tests
    lifecycle_handler.Stop();
  };
  fuchsia::hardware::bluetooth::HciHandle hci_handle =
      bthost::CreateHciHandle(config.device_path());
  if (!hci_handle) {
    bt_log(ERROR, "bt-host", "Failed to create HciHandle; cannot initialize bt-host");
    return 1;
  }

  bool initialize_res = host->Initialize(std::move(hci_handle), init_cb, error_cb);
  if (!initialize_res) {
    bt_log(ERROR, "bt-host", "Error initializing bt-host; shutting down...");
    return 1;
  }

  fidl::InterfaceRequestHandler<bthost::BtHostComponent> handler =
      [&host](fidl::InterfaceRequest<bthost::BtHostComponent> request) {
        host->BindHostInterface(request.TakeChannel());
      };
  auto context = sys::ComponentContext::CreateAndServeOutgoingDirectory();
  context->outgoing()->AddPublicService(std::move(handler), OUTGOING_SERVICE_NAME);

  loop.Run();
  return 0;
}

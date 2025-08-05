// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.virtualization.guest.interaction/cpp/fidl.h>
#include <fidl/fuchsia.virtualization/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>

#include "src/virtualization/lib/guest_interaction/interactive_debian_guest/interactive_debian_guest_impl.h"

using DebianGuestManager = fuchsia_virtualization::DebianGuestManager;
using InteractiveDebianGuest = fuchsia_virtualization_guest_interaction::InteractiveDebianGuest;

int main() {
  FX_LOGS(INFO) << "Bootstrapping the InteractiveDebianGuest component.";
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();
  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);

  // Spin up and connect to the GuestManager
  zx::result result = outgoing.ServeFromStartupInfo();
  FX_CHECK(result.is_ok()) << std::format("Failed to serve outgoing directory with status: {}",
                                          result.status_string());
  zx::result guest_manager_client_end = component::Connect<DebianGuestManager>();
  FX_CHECK(guest_manager_client_end.is_ok())
      << std::format("Failed to connect to the DebianGuestManager protocol with status: {}",
                     guest_manager_client_end.status_string());
  fidl::SyncClient<DebianGuestManager> guest_manager_sync =
      fidl::SyncClient<DebianGuestManager>({std::move(*guest_manager_client_end)});

  // Create and serve the InteractiveDebianGuest
  bool already_bound = false;
  interactive_debian_guest::InteractiveDebianGuestImpl interactive_debian_guest_impl(
      loop, std::move(guest_manager_sync));
  const auto incoming_request_handler = [dispatcher, &interactive_debian_guest_impl,
                                         &already_bound](
                                            fidl::ServerEnd<InteractiveDebianGuest> server_end) {
    FX_LOGS(INFO) << "Incoming connection for "
                  << fidl::DiscoverableProtocolName<InteractiveDebianGuest>;

    if (already_bound) {
      FX_LOGS(ERROR)
          << "The interactive Debian guest cannot gracefully handle multiple requests, and is already bound. Closing the incoming request.";
      server_end.Close(ZX_ERR_ALREADY_BOUND);
      return;
    }

    fidl::BindServer(dispatcher, std::move(server_end), &interactive_debian_guest_impl);
    already_bound = true;
  };
  result = outgoing.AddUnmanagedProtocol<InteractiveDebianGuest>(incoming_request_handler);
  FX_CHECK(result.is_ok()) << std::format(
      "Failed to register InteractiveDebianGuest protocol with status: {}", result.status_string());

  FX_LOGS(INFO) << "Running the InteractiveDebianGuest component.";
  return loop.Run();
}

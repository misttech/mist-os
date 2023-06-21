// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/coordinator-provider-lib/client-hlcpp.h"

#include <lib/fdio/directory.h>
#include <lib/fpromise/bridge.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <memory>

#include "src/lib/files/directory.h"

namespace display {

namespace {

fpromise::promise<CoordinatorHandlesHlcpp> GetCoordinatorFromProviderHlcpp(
    std::shared_ptr<fuchsia::hardware::display::ProviderPtr> provider) {
  zx::channel ctrl_server, ctrl_client;
  zx_status_t status = zx::channel::create(0, &ctrl_server, &ctrl_client);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to create display coordinator channel: "
                   << zx_status_get_string(status);
    return fpromise::make_ok_promise(CoordinatorHandlesHlcpp{});
  }

  // A reference to |provider| is retained in the closure, to keep the connection open until the
  // response is received.
  fpromise::bridge<CoordinatorHandlesHlcpp> dc_handles_bridge;
  (*provider)->OpenCoordinatorForPrimary(
      ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator>(std::move(ctrl_server)),
      [provider, completer = std::move(dc_handles_bridge.completer),
       ctrl_client = std::move(ctrl_client)](zx_status_t status) mutable {
        if (status != ZX_OK) {
          FX_LOGS(ERROR) << "GetCoordinatorHlcpp() provider responded with status: "
                         << zx_status_get_string(status);
          completer.complete_ok(CoordinatorHandlesHlcpp{});
          return;
        }

        CoordinatorHandlesHlcpp handles{
            ::fidl::InterfaceHandle<fuchsia::hardware::display::Coordinator>(
                std::move(ctrl_client))};
        completer.complete_ok(std::move(handles));
      });

  return dc_handles_bridge.consumer.promise();
}

}  // namespace

fpromise::promise<CoordinatorHandlesHlcpp> GetCoordinatorHlcpp() {
  TRACE_DURATION("gfx", "GetCoordinatorHlcpp");

  // The display coordinator should always come from the fuchsia.hardware.
  // display.Provider protocol from the services (/svc) directory the current
  // component is offered.
  std::vector<std::string> contents;
  files::ReadDirContents("/svc", &contents);
  const bool has_display_provider_service =
      std::find(contents.begin(), contents.end(), "fuchsia.hardware.display.Provider") !=
      contents.end();

  if (has_display_provider_service) {
    const char* kSvcPath = "/svc/fuchsia.hardware.display.Provider";
    auto provider = std::make_shared<fuchsia::hardware::display::ProviderPtr>();
    zx_status_t status =
        fdio_service_connect(kSvcPath, provider->NewRequest().TakeChannel().release());
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "GetCoordinatorHlcpp() failed to connect to " << kSvcPath
                     << " with status: " << zx_status_get_string(status)
                     << ". Something went wrong in fake-display injection routing.";
      return fpromise::make_result_promise<CoordinatorHandlesHlcpp>(fpromise::error());
    }
    return GetCoordinatorFromProviderHlcpp(std::move(provider));
  }

  FX_LOGS(ERROR) << "No display provider given.";
  return fpromise::make_result_promise<CoordinatorHandlesHlcpp>(fpromise::error());
}

}  // namespace display

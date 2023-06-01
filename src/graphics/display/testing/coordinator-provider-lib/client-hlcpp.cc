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

#include "src/graphics/display/testing/coordinator-provider-lib/devfs-factory-hlcpp.h"
#include "src/lib/files/directory.h"

namespace display {

fpromise::promise<CoordinatorHandlesHlcpp> GetCoordinatorHlcpp(
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

fpromise::promise<CoordinatorHandlesHlcpp> GetCoordinatorHlcpp(
    DevFsCoordinatorFactoryHlcpp* devfs_provider_opener) {
  TRACE_DURATION("gfx", "GetCoordinatorHlcpp");

  // We check the environment to see if there is any fake display is provided through
  // fuchsia.hardware.display.Provider protocol. We connect to fake display if given. Otherwise, we
  // fall back to |hdcp_service_impl|.
  // TODO(fxbug.dev/73816): Change fake display injection after moving to CFv2.
  std::vector<std::string> contents;
  files::ReadDirContents("/svc", &contents);
  const bool fake_display_is_injected =
      std::find(contents.begin(), contents.end(), "fuchsia.hardware.display.Provider") !=
      contents.end();

  auto provider = std::make_shared<fuchsia::hardware::display::ProviderPtr>();
  if (fake_display_is_injected) {
    const char* kSvcPath = "/svc/fuchsia.hardware.display.Provider";
    zx_status_t status =
        fdio_service_connect(kSvcPath, provider->NewRequest().TakeChannel().release());
    if (status != ZX_OK) {
      FX_LOGS(ERROR) << "GetCoordinatorHlcpp() failed to connect to " << kSvcPath
                     << " with status: " << zx_status_get_string(status)
                     << ". Something went wrong in fake-display injection routing.";
      return fpromise::make_result_promise<CoordinatorHandlesHlcpp>(fpromise::error());
    }
  } else if (devfs_provider_opener) {
    devfs_provider_opener->BindDisplayProvider(provider->NewRequest());
  } else {
    FX_LOGS(ERROR) << "No display provider given.";
    return fpromise::make_result_promise<CoordinatorHandlesHlcpp>(fpromise::error());
  }
  return GetCoordinatorHlcpp(std::move(provider));
}

}  // namespace display

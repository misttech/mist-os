// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/coordinator-provider-lib/client-hlcpp.h"

#include <fuchsia/io/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fpromise/bridge.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <memory>

#include "lib/fidl/cpp/interface_request.h"
#include "src/lib/files/directory.h"

namespace display {

namespace {

constexpr char kSvcPath[] = "/svc/fuchsia.hardware.display.Provider";

}  // namespace

fpromise::promise<CoordinatorHandlesHlcpp, zx_status_t> GetCoordinatorHlcpp() {
  TRACE_DURATION("gfx", "GetCoordinatorHlcpp");

  fpromise::bridge<void, zx_status_t> bridge;
  std::shared_ptr completer =
      std::make_shared<decltype(bridge.completer)>(std::move(bridge.completer));

  fuchsia::io::NodePtr ptr;
  ptr.set_error_handler([completer](zx_status_t status) {
    FX_PLOGS(ERROR, status) << "failed to connect to " << kSvcPath;
    completer->complete_error(status);
  });
  ptr.events().OnOpen = [completer](
                            zx_status_t status,
                            std::unique_ptr<fuchsia::io::NodeInfoDeprecated> node_info) mutable {
    if (status != ZX_OK) {
      completer->complete_error(status);
      return;
    }
    completer->complete_ok();
  };

  if (zx_status_t status =
          fdio_open(kSvcPath, static_cast<uint32_t>(fuchsia::io::OpenFlags::DESCRIBE),
                    ptr.NewRequest().TakeChannel().release());
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "failed to connect to " << kSvcPath;
    completer->complete_error(status);
  }

  return bridge.consumer.promise().and_then([ptr = std::move(ptr)]() mutable {
    fpromise::bridge<CoordinatorHandlesHlcpp, zx_status_t> bridge;
    std::shared_ptr completer =
        std::make_shared<decltype(bridge.completer)>(std::move(bridge.completer));

    fuchsia::hardware::display::ProviderPtr provider;
    provider.set_error_handler([completer](zx_status_t status) {
      FX_PLOGS(ERROR, status) << "failed to connect to " << kSvcPath;
      completer->complete_error(status);
    });
    provider.Bind(ptr.Unbind().TakeChannel());
    fidl::InterfaceHandle<fuchsia::hardware::display::Coordinator> coordinator;
    provider->OpenCoordinatorForPrimary(
        coordinator.NewRequest(),
        [provider = std::move(provider), completer = std::move(completer),
         coordinator = std::move(coordinator)](zx_status_t status) mutable {
          if (status != ZX_OK) {
            FX_PLOGS(ERROR, status) << "OpenCoordinatorForPrimary error";
            completer->complete_error(status);
            return;
          }

          completer->complete_ok({
              .coordinator = std::move(coordinator),
          });
        });
    return bridge.consumer.promise();
  });
}

}  // namespace display

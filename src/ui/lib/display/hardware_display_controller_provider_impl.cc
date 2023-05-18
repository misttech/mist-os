// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/lib/display/hardware_display_controller_provider_impl.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <cstdint>

#include "src/lib/fsl/io/device_watcher.h"

namespace ui_display {

static const std::string kDisplayDir = "/dev/class/display-coordinator";

HardwareDisplayCoordinatorProviderImpl::HardwareDisplayCoordinatorProviderImpl(
    sys::ComponentContext* app_context) {
  app_context->outgoing()->AddPublicService(bindings_.GetHandler(this));
}

// |fuchsia::hardware::display::Provider|.
void HardwareDisplayCoordinatorProviderImpl::OpenCoordinatorForPrimary(
    ::fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator> request,
    OpenCoordinatorForPrimaryCallback callback) {
  // Watcher's lifetime needs to be at most as long as the lifetime of |this|,
  // and otherwise as long as the lifetime of |callback|.  |this| will own
  // the references to outstanding watchers, and each watcher will notify |this|
  // when it is done, so that |this| can remove a reference to it.
  static uint64_t last_id = 0;
  const uint64_t id = ++last_id;

  std::unique_ptr<fsl::DeviceWatcher> watcher = fsl::DeviceWatcher::Create(
      kDisplayDir,
      [id, holders = &holders_, request = std::move(request), callback = std::move(callback)](
          const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) mutable {
        FX_LOGS(INFO) << "Found display controller at path: " << kDisplayDir << '/' << filename
                      << '.';

        zx::result client = component::ConnectAt<fuchsia_hardware_display::Provider>(dir, filename);
        if (client.is_error()) {
          FX_PLOGS(ERROR, client.error_value())
              << "Failed to open display_controller at path: " << kDisplayDir << '/' << filename;

          // We could try to match the value of the C "errno" macro to the closest ZX error, but
          // this would give rise to many corner cases.  We never expect this to fail anyway, since
          // |filename| is given to us by the device watcher.
          callback(ZX_ERR_INTERNAL);
          return;
        }

        // TODO(fxbug.dev/57269): it would be nice to simply pass |callback| asynchronously into
        // OpenCoordinator(), rather than blocking on a synchronous call.  However, it is
        // non-trivial to do so, so for now we use a blocking call to proxy the request.
        fidl::WireResult result =
            fidl::WireCall(client.value())
                ->OpenCoordinatorForPrimary(
                    fidl::ServerEnd<fuchsia_hardware_display::Coordinator>(request.TakeChannel()));
        if (!result.ok()) {
          FX_PLOGS(ERROR, result.status()) << "Failed to call service handle";

          // There's not a clearly-better value to return here.  Returning the FIDL error would be
          // somewhat unexpected, since the caller wouldn't receive it as a FIDL status, rather as
          // the return value of a "successful" method invocation.
          callback(ZX_ERR_INTERNAL);
          return;
        }
        if (result->s != ZX_OK) {
          FX_PLOGS(ERROR, result->s) << "Failed to open display coordinator";
          callback(result->s);
          return;
        }

        callback(ZX_OK);

        // We no longer need |this| to store this closure, remove it. Do not do
        // any work after this point.
        holders->erase(id);
      });
  holders_[id] = std::move(watcher);
}

void HardwareDisplayCoordinatorProviderImpl::BindDisplayProvider(
    fidl::InterfaceRequest<fuchsia::hardware::display::Provider> request) {
  bindings_.AddBinding(this, std::move(request));
}

}  // namespace ui_display

// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/testing/coordinator-connector-lib/devfs-factory.h"

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>
#include <zircon/status.h>

#include <cstdint>

#include "src/lib/fsl/io/device_watcher.h"

namespace display {

static const std::string kDisplayDir = "/dev/class/display-coordinator";

zx::result<> DevFsCoordinatorFactory::CreateAndPublishService(
    component::OutgoingDirectory& outgoing, async_dispatcher_t* dispatcher) {
  return outgoing.AddProtocol<fuchsia_hardware_display::Provider>(
      std::make_unique<DevFsCoordinatorFactory>(dispatcher));
}

DevFsCoordinatorFactory::DevFsCoordinatorFactory(async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher) {}

void DevFsCoordinatorFactory::OpenCoordinatorForVirtcon(
    OpenCoordinatorForVirtconRequest& request,
    OpenCoordinatorForVirtconCompleter::Sync& completer) {
  completer.Reply({{
      .s = ZX_ERR_NOT_SUPPORTED,
  }});
}

zx_status_t DevFsCoordinatorFactory::OpenCoordinatorForPrimaryOnDevice(
    const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename,
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server) {
  zx::result client = component::ConnectAt<fuchsia_hardware_display::Provider>(dir, filename);
  if (client.is_error()) {
    FX_PLOGS(ERROR, client.error_value())
        << "Failed to open display_controller at path: " << kDisplayDir << '/' << filename;

    // We could try to match the value of the C "errno" macro to the closest ZX error, but
    // this would give rise to many corner cases.  We never expect this to fail anyway, since
    // |filename| is given to us by the device watcher.
    return ZX_ERR_INTERNAL;
  }

  // TODO(fxbug.dev/57269): Pass an async completer asynchronously into
  // OpenCoordinator(), rather than blocking on a synchronous call.
  fidl::WireResult result =
      fidl::WireCall(client.value())->OpenCoordinatorForPrimary(std::move(coordinator_server));
  if (!result.ok()) {
    FX_PLOGS(ERROR, result.status()) << "Failed to call service handle";

    // There's not a clearly-better value to return here.  Returning the FIDL error would be
    // somewhat unexpected, since the caller wouldn't receive it as a FIDL status, rather as
    // the return value of a "successful" method invocation.
    return ZX_ERR_INTERNAL;
  }
  if (result->s != ZX_OK) {
    FX_PLOGS(ERROR, result->s) << "Failed to open display coordinator";
    return result->s;
  }

  return ZX_OK;
}

void DevFsCoordinatorFactory::OpenCoordinatorForPrimary(
    OpenCoordinatorForPrimaryRequest& request,
    OpenCoordinatorForPrimaryCompleter::Sync& completer) {
  // Watcher's lifetime needs to be at most as long as the lifetime of |this|,
  // and otherwise as long as the lifetime of |callback|.  |this| will own
  // the references to outstanding watchers, and each watcher will notify |this|
  // when it is done, so that |this| can remove a reference to it.
  const int64_t id = next_display_client_id_++;

  std::unique_ptr<fsl::DeviceWatcher> watcher = fsl::DeviceWatcher::Create(
      kDisplayDir,
      [this, id, request = std::move(request), async_completer = completer.ToAsync()](
          const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) mutable {
        FX_LOGS(INFO) << "Found display controller at path: " << kDisplayDir << '/' << filename
                      << '.';
        zx_status_t open_coordinator_status =
            OpenCoordinatorForPrimaryOnDevice(dir, filename, std::move(request.coordinator()));
        if (open_coordinator_status != ZX_OK) {
          async_completer.Reply({{.s = open_coordinator_status}});
          return;
        }
        async_completer.Reply({{.s = ZX_OK}});
        // We no longer need |this| to store this closure, remove it. Do not do
        // any work after this point.
        pending_device_watchers_.erase(id);
      },
      dispatcher_);
  pending_device_watchers_[id] = std::move(watcher);
}

}  // namespace display

// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.compat/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/internal/symbols.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <zircon/errors.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "src/ui/input/drivers/hid-input-report/input-report.h"

namespace fdf2 = fuchsia_driver_framework;
namespace fio = fuchsia_io;

namespace {

const std::string kDeviceName = "InputReport";

class InputReportDriver : public fdf::DriverBase {
 public:
  InputReportDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : DriverBase(kDeviceName, std::move(start_args), std::move(dispatcher)),
        devfs_connector_(fit::bind_member<&InputReportDriver::Serve>(this)) {}

  zx::result<> Start() override {
    zx::result client_result = compat::ConnectBanjo<ddk::HidDeviceProtocolClient>(incoming());
    if (client_result.is_error()) {
      FDF_LOG(ERROR, "Failed to connect to HidDeviceProtocolClient. %s",
              client_result.status_string());
      return client_result.take_error();
    }

    ddk::HidDeviceProtocolClient hiddev = client_result.value();
    if (!hiddev.is_valid()) {
      FDF_LOG(ERROR, "Failed to create hiddev");
      return zx::error(ZX_ERR_INTERNAL);
    }
    input_report_.emplace(std::move(hiddev));

    // Expose the driver's inspect data.
    InitInspectorExactlyOnce(input_report_->Inspector());

    // Start the inner DFv1 driver.
    input_report_->Start();

    // Export our InputReport protocol.
    auto status = outgoing()->component().AddUnmanagedProtocol<fuchsia_input_report::InputDevice>(
        input_report_bindings_.CreateHandler(&input_report_.value(), dispatcher(),
                                             fidl::kIgnoreBindingClosure),
        kDeviceName);
    if (status.is_error()) {
      return status.take_error();
    }

    if (zx::result result = CreateDevfsNode(); result.is_error()) {
      return result.take_error();
    }

    return zx::ok();
  }

 private:
  zx::result<> CreateDevfsNode() {
    fidl::Arena arena;
    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                     .connector(std::move(connector.value()))
                     .class_name("input-report");

    auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                    .name(arena, kDeviceName)
                    .devfs_args(devfs.Build())
                    .Build();

    // Create endpoints of the `NodeController` for the node.
    auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

    zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ZX_ASSERT_MSG(node_endpoints.is_ok(), "Failed to create endpoints: %s",
                  node_endpoints.status_string());

    fidl::WireResult result = fidl::WireCall(node())->AddChild(
        args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
    if (!result.ok()) {
      FDF_SLOG(ERROR, "Failed to add child", KV("status", result.status_string()));
      return zx::error(result.status());
    }
    controller_.Bind(std::move(controller_endpoints.client));
    node_.Bind(std::move(node_endpoints->client));
    return zx::ok();
  }

  void Serve(fidl::ServerEnd<fuchsia_input_report::InputDevice> server) {
    input_report_bindings_.AddBinding(dispatcher(), std::move(server), &input_report_.value(),
                                      fidl::kIgnoreBindingClosure);
  }

  // Calling this function drops our node handle, which tells the DriverFramework to call Stop
  // on the Driver.
  void ScheduleStop() { node().reset(); }

  std::optional<hid_input_report_dev::InputReport> input_report_;
  fidl::ServerBindingGroup<fuchsia_input_report::InputDevice> input_report_bindings_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  driver_devfs::Connector<fuchsia_input_report::InputDevice> devfs_connector_;
};

}  // namespace

// TODO(https://fxbug.dev/42176828): Figure out how to get logging working.
zx_driver_rec_t __zircon_driver_rec__ = {};

void driver_logf_internal(const zx_driver_t* drv, fx_log_severity_t severity, const char* tag,
                          const char* file, int line, const char* msg, ...) {}

bool driver_log_severity_enabled_internal(const zx_driver_t* drv, fx_log_severity_t severity) {
  return true;
}

FUCHSIA_DRIVER_EXPORT(InputReportDriver);

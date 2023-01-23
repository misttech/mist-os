// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.compat/cpp/fidl.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/debug.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/compat/cpp/context.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/driver_cpp.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/inspect/component/cpp/component.h>
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
    auto parent_symbol = fdf::GetSymbol<compat::device_t*>(symbols(), compat::kDeviceSymbol);

    hid_device_protocol_t proto = {};
    if (parent_symbol->proto_ops.id != ZX_PROTOCOL_HID_DEVICE) {
      FDF_LOG(ERROR, "Didn't find HID_DEVICE protocol");
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    proto.ctx = parent_symbol->context;
    proto.ops = reinterpret_cast<const hid_device_protocol_ops_t*>(parent_symbol->proto_ops.ops);

    ddk::HidDeviceProtocolClient hiddev(&proto);
    if (!hiddev.is_valid()) {
      FDF_LOG(ERROR, "Failed to create hiddev");
      return zx::error(ZX_ERR_INTERNAL);
    }
    input_report_.emplace(std::move(hiddev));

    // Expose the driver's inspect data.
    exposed_inspector_.emplace(inspect::ComponentInspector(
        context().outgoing()->component(), dispatcher(), input_report_->Inspector()));

    // Start the inner DFv1 driver.
    input_report_->Start();

    // Export our InputReport protocol.
    auto status =
        context().outgoing()->component().AddUnmanagedProtocol<fuchsia_input_report::InputDevice>(
            input_report_bindings_.CreateHandler(&input_report_.value(), dispatcher(),
                                                 fidl::kIgnoreBindingClosure),
            kDeviceName);
    if (status.is_error()) {
      return status.take_error();
    }

    // Create our compat context, and serve our device when it's created.
    compat::Context::ConnectAndCreate(
        &context(), dispatcher(),
        fit::bind_member(this, &InputReportDriver::CreateAndExportDevice));
    return zx::ok();
  }

 private:
  void CreateAndExportDevice(zx::result<std::shared_ptr<compat::Context>> context) {
    if (!context.is_ok()) {
      FDF_LOG(ERROR, "Call to Context::ConnectAndCreate failed: %s", context.status_string());
      return ScheduleStop();
    }
    compat_context_ = std::move(*context);

    // Export to devfs.
    zx::result connection = this->context().incoming()->Connect<fuchsia_device_fs::Exporter>();
    if (connection.is_error()) {
      FDF_SLOG(ERROR, "Failed to connect to fuchsia_device_fs::Exporter",
               KV("status", connection.status_string()));
      return ScheduleStop();
    }

    fidl::WireSyncClient devfs_exporter{std::move(connection.value())};

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      FDF_SLOG(ERROR, "Failed to bind devfs_connector: %s",
               KV("status", connector.status_string()));
      return ScheduleStop();
    }
    fidl::WireResult export_result = devfs_exporter->ExportV2(
        std::move(connector.value()),
        fidl::StringView::FromExternal(compat_context_->TopologicalPath(kDeviceName)),
        fidl::StringView::FromExternal("input-report"), fuchsia_device_fs::ExportOptions());
    if (!export_result.ok()) {
      FDF_SLOG(ERROR, "Failed to export to devfs: %s", KV("status", export_result.status_string()));
      return ScheduleStop();
    }
    if (export_result.value().is_error()) {
      FDF_SLOG(ERROR, "Failed to export to devfs: %s",
               KV("status", zx_status_get_string(export_result.value().error_value())));
      return ScheduleStop();
    }
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
  std::optional<inspect::ComponentInspector> exposed_inspector_;
  std::shared_ptr<compat::Context> compat_context_;
  driver_devfs::Connector<fuchsia_input_report::InputDevice> devfs_connector_;
};

}  // namespace

// TODO(fxbug.dev/94884): Figure out how to get logging working.
zx_driver_rec_t __zircon_driver_rec__ = {};

void driver_logf_internal(const zx_driver_t* drv, fx_log_severity_t severity, const char* tag,
                          const char* file, int line, const char* msg, ...) {}

bool driver_log_severity_enabled_internal(const zx_driver_t* drv, fx_log_severity_t severity) {
  return true;
}

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<InputReportDriver>);

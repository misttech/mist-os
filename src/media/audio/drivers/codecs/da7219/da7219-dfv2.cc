// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/codecs/da7219/da7219-dfv2.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <fidl/fuchsia.hardware.acpi/cpp/wire.h>
#include <lib/driver/component/cpp/service_client.h>
#include <lib/fit/defer.h>

namespace audio::da7219 {

zx::result<zx::interrupt> Driver::GetIrq() const {
  auto acpi_client = context().incoming()->Connect<fuchsia_hardware_acpi::Service::Device>("acpi");
  if (!acpi_client.is_ok()) {
    return acpi_client.take_error();
  }
  auto irq = fidl::WireCall(acpi_client.value())->MapInterrupt(0);
  if (!irq.ok() || irq.value().is_error()) {
    DA7219_LOG(ERROR, "Could not get IRQ: %s",
               irq.ok() ? zx_status_get_string(irq.value().error_value())
                        : irq.FormatDescription().data());
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  return zx::ok(std::move(irq.value().value()->irq));
}

zx::result<> Driver::Start() {
  zx::result i2c = context().incoming()->Connect<fuchsia_hardware_i2c::Service::Device>("i2c000");
  if (!i2c.is_ok()) {
    DA7219_LOG(ERROR, "Could not get I2C client: %s", i2c.status_string());
    return i2c.take_error();
  }
  zx::result irq = GetIrq();
  if (!irq.is_ok()) {
    DA7219_LOG(ERROR, "Could not get IRQ: %s", irq.status_string());
    return irq.take_error();
  }

  // There is a core class that implements the core logic and interaction with the hardware, and a
  // Server class that allows the creation of multiple instances (one for input and one for output).
  core_ = std::make_shared<Core>(logger_.get(), std::move(i2c.value()), std::move(irq.value()));
  if (zx_status_t status = core_->Initialize(); status != ZX_OK) {
    DA7219_LOG(ERROR, "Could not initialize: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  if (zx::result output = Serve(std::string(name()).append("-output"), false); output.is_error()) {
    FDF_SLOG(ERROR, "Could not serve output server", KV("status", output.status_string()));
    return output.take_error();
  }

  if (zx::result input = Serve(std::string(name()).append("-input"), true); input.is_error()) {
    FDF_SLOG(ERROR, "Could not serve input server", KV("status", input.status_string()));
    return input.take_error();
  }

  FDF_SLOG(INFO, "Started");

  return zx::ok();
}

zx::result<> Driver::Serve(std::string_view name, bool is_input) {
  // Serve the fuchsia.hardware.audio/CodecConnector protocol to clients through the
  // fuchsia.hardware.audio/CodecConnectorService wrapper.
  fuchsia_hardware_audio::CodecConnectorService::InstanceHandler handler;

  // For a given instance of this driver only one client is supported at the time, however before
  // any client connects to the server the framework opens the corresponding devfs nodes creating
  // a first connection to CodecConnector.
  // Since the framework requires allowing and keeping this connection in addition to the connection
  // from a client, we allow multiple bindings of ServerConnector but share the instance.
  // ServerConnector limits the actual usage to one client at the time.
  if (is_input) {
    server_input_ = std::make_shared<ServerConnector>(logger_.get(), core_, true);
  } else {
    server_output_ = std::make_shared<ServerConnector>(logger_.get(), core_, false);
  }

  auto result = handler.add_codec_connector(fit::bind_member<&ServerConnector::BindConnector>(
      is_input ? server_input_.get() : server_output_.get()));
  ZX_ASSERT(result.is_ok());

  result = context().outgoing()->AddService<fuchsia_hardware_audio::CodecConnectorService>(
      std::move(handler), name);
  if (result.is_error()) {
    FDF_SLOG(ERROR, "Failed to add service", KV("status", result.status_string()));
    return result.take_error();
  }

  // Serve using devfs.
  compat::Context::ConnectAndCreate(
      &context(), dispatcher(),
      [this, is_input,
       name_copy = std::string(name)](zx::result<std::unique_ptr<compat::Context>> result) mutable {
        // If we hit an error this will reset our node which signals that the driver framework
        // should stop us.
        auto reset_node = fit::defer([this]() { node().reset(); });

        if (result.is_error()) {
          FDF_SLOG(ERROR, "Failed to get compat::Context", KV("status", result.status_string()));
          return;
        }
        compat_context_ = std::move(result.value());
        auto devfs_path = compat_context_->TopologicalPath(name_copy);

        zx::result connection = context().incoming()->Connect<fuchsia_device_fs::Exporter>();
        if (connection.is_error()) {
          FDF_SLOG(ERROR, "Failed to connect to fuchsia_device_fs::Exporter",
                   KV("status", connection.status_string()));
          return;
        }

        fidl::WireSyncClient devfs_exporter{std::move(connection.value())};
        ServerConnector& server = is_input ? *server_input_.get() : *server_output_.get();

        zx::result connector = server.devfs_connector().Bind(dispatcher());
        if (connector.is_error()) {
          FDF_SLOG(ERROR, "Failed to bind devfs_connector: %s",
                   KV("status", connector.status_string()));
          return;
        }
        fidl::WireResult export_result = devfs_exporter->ExportV2(
            std::move(connector.value()), fidl::StringView::FromExternal(devfs_path),
            fidl::StringView::FromExternal("codec"), fuchsia_device_fs::ExportOptions());
        if (!export_result.ok()) {
          FDF_SLOG(ERROR, "Failed to export to devfs: %s",
                   KV("status", export_result.status_string()));
          return;
        }
        if (export_result.value().is_error()) {
          FDF_SLOG(ERROR, "Failed to export to devfs: %s",
                   KV("status", zx_status_get_string(export_result.value().error_value())));
          return;
        }

        reset_node.cancel();
        FDF_SLOG(INFO, "Exported", KV("devfs_path", devfs_path.c_str()));
      });
  return zx::ok();
}

}  // namespace audio::da7219

FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<audio::da7219::Driver>);

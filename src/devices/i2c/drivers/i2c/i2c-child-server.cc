// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/i2c/drivers/i2c/i2c-child-server.h"

#include <lib/driver/node/cpp/add_child.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>

namespace i2c {

zx::result<std::unique_ptr<I2cChildServer>> I2cChildServer::CreateAndAddChild(
    OnTransact on_transact, fidl::ClientEnd<fuchsia_driver_framework::Node>& node_client,
    fdf::Logger& logger, uint32_t bus_id, const fuchsia_hardware_i2c_businfo::I2CChannel& channel,
    const std::shared_ptr<fdf::Namespace>& incoming,
    const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
    const std::optional<std::string>& parent_node_name) {
  const uint32_t i2c_class = channel.i2c_class().value_or(0);
  const uint32_t vid = channel.vid().value_or(0);
  const uint32_t pid = channel.pid().value_or(0);
  const uint32_t did = channel.did().value_or(0);
  const uint32_t address = channel.address().value_or(0);
  const std::string friendly_name = channel.name().value_or("");

  char child_name[32];
  snprintf(child_name, sizeof(child_name), "i2c-%u-%u", bus_id, address);

  // Set up the compat server.
  auto compat_server = std::make_unique<compat::SyncInitializedDeviceServer>();
  {
    zx::result<> result =
        compat_server->Initialize(incoming, outgoing, parent_node_name, child_name);
    if (result.is_error()) {
      return result.take_error();
    }
  }
  // Create the I2cChildServer.
  auto i2c_child_server = std::make_unique<I2cChildServer>(
      std::move(on_transact), std::move(compat_server), address, friendly_name);
  auto serve_result = outgoing->AddService<fuchsia_hardware_i2c::Service>(
      fuchsia_hardware_i2c::Service::InstanceHandler({
          .device = i2c_child_server->bindings_.CreateHandler(
              i2c_child_server.get(), fdf::Dispatcher::GetCurrent()->async_dispatcher(),
              fidl::kIgnoreBindingClosure),
      }),
      child_name);
  if (serve_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", serve_result.status_string());
    return serve_result.take_error();
  }

  // Set up the devfs connector.
  zx::result connector =
      i2c_child_server->devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connector. %s", connector.status_string());
    return connector.take_error();
  }

  // Add the child node.
  std::vector<fuchsia_driver_framework::NodeProperty> properties{
      fdf::MakeProperty(bind_fuchsia::I2C_BUS_ID, bus_id),
      fdf::MakeProperty(bind_fuchsia::I2C_ADDRESS, static_cast<uint32_t>(address)),
      fdf::MakeProperty(bind_fuchsia::I2C_CLASS, i2c_class),
  };

  if (vid || pid || did) {
    properties.push_back(fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID, vid));
    properties.push_back(fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_PID, pid));
    properties.push_back(fdf::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID, did));
  }

  auto offers = i2c_child_server->compat_server_->CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fidl_i2c::Service>(child_name));

  fuchsia_driver_framework::DevfsAddArgs devfs_args{
      {.connector = std::move(connector.value()), .class_name = "i2c"}};
  zx::result controller =
      fdf::AddChild(node_client, logger, child_name, devfs_args, properties, offers);
  if (controller.is_error()) {
    FDF_LOG(ERROR, "Failed to add child %s.", controller.status_string());
    return controller.take_error();
  }

  return zx::ok(std::move(i2c_child_server));
}

void I2cChildServer::Transfer(TransferRequestView request, TransferCompleter::Sync& completer) {
  on_transact_(address_, request, completer);
}

void I2cChildServer::GetName(GetNameCompleter::Sync& completer) {
  if (name_.empty()) {
    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  completer.ReplySuccess(fidl::StringView::FromExternal(name_));
}

}  // namespace i2c

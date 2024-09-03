// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_SERVER_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_SERVER_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/fidl.h>
#include <fidl/fuchsia.hardware.i2c/cpp/fidl.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/logger.h>

namespace i2c {

namespace fidl_i2c = fuchsia_hardware_i2c;

class I2cChildServer : public fidl::WireServer<fidl_i2c::Device> {
 public:
  using OnTransact = fit::function<void(uint16_t address, TransferRequestView request,
                                        TransferCompleter::Sync& completer)>;

  I2cChildServer(OnTransact on_transact,
                 std::unique_ptr<compat::SyncInitializedDeviceServer> compat_server,
                 uint16_t address, const std::string& name)
      : on_transact_(std::move(on_transact)),
        address_(address),
        name_(name),
        compat_server_(std::move(compat_server)) {}

  static zx::result<std::unique_ptr<I2cChildServer>> CreateAndAddChild(
      OnTransact on_transact, fidl::ClientEnd<fuchsia_driver_framework::Node>& node_client,
      fdf::Logger& logger, uint32_t bus_id, const fuchsia_hardware_i2c_businfo::I2CChannel& channel,
      const std::shared_ptr<fdf::Namespace>& incoming,
      const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
      const std::optional<std::string>& parent_node_name);

  void Transfer(TransferRequestView request, TransferCompleter::Sync& completer) override;
  void GetName(GetNameCompleter::Sync& completer) override;

 private:
  void Serve(fidl::ServerEnd<fidl_i2c::Device> request) {
    bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request),
                         this, fidl::kIgnoreBindingClosure);
  }

  OnTransact on_transact_;
  const uint16_t address_;
  const std::string name_;

  std::unique_ptr<compat::SyncInitializedDeviceServer> compat_server_;
  driver_devfs::Connector<fidl_i2c::Device> devfs_connector_{
      fit::bind_member<&I2cChildServer::Serve>(this)};
  fidl::ServerBindingGroup<fidl_i2c::Device> bindings_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_SERVER_H_

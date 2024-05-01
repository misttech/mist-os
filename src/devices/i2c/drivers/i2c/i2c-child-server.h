// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_SERVER_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_SERVER_H_

#include <fidl/fuchsia.hardware.i2c/cpp/fidl.h>
#include <lib/driver/compat/cpp/compat.h>

#include "src/devices/i2c/drivers/i2c/i2c.h"

namespace i2c {

namespace fidl_i2c = fuchsia_hardware_i2c;

class I2cDriver;

class I2cChildServer : public fidl::WireServer<fidl_i2c::Device> {
 public:
  I2cChildServer(I2cDriver* owner,
                 std::unique_ptr<compat::SyncInitializedDeviceServer> compat_server,
                 uint16_t address, const std::string& name)
      : owner_(owner), address_(address), name_(name), compat_server_(std::move(compat_server)) {}

  static zx::result<std::unique_ptr<I2cChildServer>> CreateAndAddChild(
      I2cDriver* owner, fidl::WireSyncClient<fuchsia_driver_framework::Node>& node_client,
      const uint32_t bus_id, const fuchsia_hardware_i2c_businfo::I2CChannel& channel,
      const std::shared_ptr<fdf::Namespace>& incoming,
      const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
      const std::optional<std::string>& parent_node_name);

  void Transfer(TransferRequestView request, TransferCompleter::Sync& completer) override;
  void GetName(GetNameCompleter::Sync& completer) override;

 private:
  I2cDriver* const owner_;
  const uint16_t address_;
  const std::string name_;

  std::unique_ptr<compat::SyncInitializedDeviceServer> compat_server_;
  fidl::ServerBindingGroup<fidl_i2c::Device> bindings_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_SERVER_H_

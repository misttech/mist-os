// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <string>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/ref_ptr.h>

#include "i2c-bus.h"

namespace i2c {

namespace fidl_i2c = fuchsia_hardware_i2c;

class I2cChild;
using I2cChildType = ddk::Device<I2cChild, ddk::Messageable<fidl_i2c::Device>::Mixin>;

class I2cChild : public I2cChildType {
 public:
  I2cChild(zx_device_t* parent, fbl::RefPtr<I2cBus> bus, uint16_t address,
           async_dispatcher_t* dispatcher, const std::string& name)
      : I2cChildType(parent),
        outgoing_dir_(dispatcher),
        bus_(std::move(bus)),
        address_(address),
        name_(name),
        dispatcher_(dispatcher) {}

  static zx_status_t CreateAndAddDevice(
      zx_device_t* parent, const fuchsia_hardware_i2c_businfo::wire::I2CChannel& channel,
      const fbl::RefPtr<I2cBus>& bus, async_dispatcher_t* dispatcher);

  void DdkRelease() { delete this; }

  void Transfer(TransferRequestView request, TransferCompleter::Sync& completer) override;

  void GetName(GetNameCompleter::Sync& completer) override;

 private:
  friend class I2cChildTest;

  void Bind(fidl::ServerEnd<fidl_i2c::Device> request) {
    fidl::BindServer(dispatcher_, std::move(request), this);
  }

  component::OutgoingDirectory outgoing_dir_;

  fbl::RefPtr<I2cBus> bus_;
  const uint16_t address_;
  const std::string name_;
  async_dispatcher_t* const dispatcher_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_H_

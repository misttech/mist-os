// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

#include <string>

#include <ddktl/device.h>
#include <ddktl/fidl.h>

#include "i2c.h"
#include "lib/fdf/dispatcher.h"

namespace i2c {

namespace fidl_i2c = fuchsia_hardware_i2c;

class I2cChild;
using I2cChildType = ddk::Device<I2cChild, ddk::Messageable<fidl_i2c::Device>::Mixin>;

class I2cChild : public I2cChildType {
 public:
  I2cChild(I2cDevice* parent, uint16_t address, const std::string& name)
      : I2cChildType(parent->zxdev()),
        outgoing_dir_(fdf::Dispatcher::GetCurrent()->async_dispatcher()),
        parent_(parent),
        address_(address),
        name_(name) {}

  static zx_status_t CreateAndAddDevice(
      uint32_t bus_id, const fuchsia_hardware_i2c_businfo::wire::I2CChannel& channel,
      I2cDevice* parent);

  void DdkRelease();

  void Transfer(TransferRequestView request, TransferCompleter::Sync& completer) override;

  void GetName(GetNameCompleter::Sync& completer) override;

 private:
  friend class I2cChildTest;

  void Bind(fidl::ServerEnd<fidl_i2c::Device> request) {
    bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(request),
                         this, fidl::kIgnoreBindingClosure);
  }

  component::OutgoingDirectory outgoing_dir_;

  I2cDevice* const parent_;
  const uint16_t address_;
  const std::string name_;
  fidl::ServerBindingGroup<fidl_i2c::Device> bindings_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_CHILD_H_

// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_
#define SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fidl/fuchsia.hardware.serialimpl/cpp/driver/wire.h>
#include <fuchsia/hardware/serialimpl/cpp/banjo.h>
#include <lib/ddk/driver.h>
#include <zircon/types.h>

#include <variant>

#include <ddktl/device.h>
#include <ddktl/fidl.h>

namespace serial {

class SerialDevice;
using DeviceType =
    ddk::Device<SerialDevice, ddk::Messageable<fuchsia_hardware_serial::DeviceProxy>::Mixin,
                ddk::Unbindable>;

class SerialDevice : public DeviceType, public fidl::WireServer<fuchsia_hardware_serial::Device> {
 public:
  using SerialType = std::variant<fdf::WireClient<fuchsia_hardware_serialimpl::Device>,
                                  ddk::SerialImplProtocolClient>;

  SerialDevice(zx_device_t* parent, SerialType serial)
      : DeviceType(parent), serial_(std::move(serial)) {}

  static zx_status_t Create(void* ctx, zx_device_t* dev);
  zx_status_t Bind();
  zx_status_t Init();

  // Device protocol implementation.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  void GetChannel(GetChannelRequestView request, GetChannelCompleter::Sync& completer) override;

  void Read(ReadCompleter::Sync& completer) override;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) override;

 private:
  // Fidl protocol implementation.
  void GetClass(GetClassCompleter::Sync& completer) override;
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override;

  zx_status_t Enable(bool enable);

  // The serial protocol of the device we are binding against.
  // TODO(b/333094948): Drop support for Banjo.
  SerialType serial_;

  uint32_t serial_class_;
  using Binding = struct {
    fidl::ServerBindingRef<fuchsia_hardware_serial::Device> binding;
    std::optional<ddk::UnbindTxn> unbind_txn;
  };
  std::optional<Binding> binding_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_

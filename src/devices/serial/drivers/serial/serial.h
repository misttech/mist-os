// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_
#define SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <fuchsia/hardware/serial/cpp/banjo.h>
#include <fuchsia/hardware/serialimpl/cpp/banjo.h>
#include <lib/ddk/driver.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/event.h>
#include <lib/zx/socket.h>
#include <zircon/types.h>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <fbl/mutex.h>

namespace serial {

class SerialDevice;
using DeviceType =
    ddk::Device<SerialDevice, ddk::Messageable<fuchsia_hardware_serial::DeviceProxy>::Mixin,
                ddk::Unbindable>;

class SerialDevice : public DeviceType,
                     public ddk::SerialProtocol<SerialDevice, ddk::base_protocol>,
                     public fidl::WireServer<fuchsia_hardware_serial::Device> {
 public:
  explicit SerialDevice(zx_device_t* parent) : DeviceType(parent), serial_(parent) {}

  static zx_status_t Create(void* ctx, zx_device_t* dev);
  zx_status_t Bind();
  zx_status_t Init();

  // Device protocol implementation.
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  // Serial protocol implementation.
  zx_status_t SerialGetInfo(serial_port_info_t* info);
  zx_status_t SerialConfig(uint32_t baud_rate, uint32_t flags);

  void GetChannel(GetChannelRequestView request, GetChannelCompleter::Sync& completer) override;

  void Read(ReadCompleter::Sync& completer) override;
  void Write(WriteRequestView request, WriteCompleter::Sync& completer) override;

 private:
  // Fidl protocol implementation.
  void GetClass(GetClassCompleter::Sync& completer) override;
  void SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) override;

  // The serial protocol of the device we are binding against.
  ddk::SerialImplProtocolClient serial_;

  uint32_t serial_class_;
  using Binding = struct {
    fidl::ServerBindingRef<fuchsia_hardware_serial::Device> binding;
    std::optional<ddk::UnbindTxn> unbind_txn;
  };
  std::optional<Binding> binding_;
};

}  // namespace serial

#endif  // SRC_DEVICES_SERIAL_DRIVERS_SERIAL_SERIAL_H_

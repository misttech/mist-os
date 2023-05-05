// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/fit/function.h>
#include <lib/zx/handle.h>
#include <lib/zx/time.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

namespace serial {

zx_status_t SerialDevice::SerialGetInfo(serial_port_info_t* info) { return serial_.GetInfo(info); }

zx_status_t SerialDevice::SerialConfig(uint32_t baud_rate, uint32_t flags) {
  return serial_.Config(baud_rate, flags);
}

void SerialDevice::Read(ReadCompleter::Sync& completer) {
  uint8_t data[fuchsia_io::wire::kMaxBuf];
  size_t actual;
  if (zx_status_t status = serial_.Read(data, sizeof(data), &actual); status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(data, actual));
}

void SerialDevice::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  cpp20::span data = request->data.get();
  while (!data.empty()) {
    size_t actual;
    if (zx_status_t status = serial_.Write(data.data(), data.size_bytes(), &actual);
        status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    data = data.subspan(actual);
  }

  completer.ReplySuccess();
}

void SerialDevice::GetChannel(GetChannelRequestView request, GetChannelCompleter::Sync& completer) {
  if (binding_.has_value()) {
    request->req.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }

  if (zx_status_t status = serial_.Enable(true); status != ZX_OK) {
    request->req.Close(status);
    return;
  }

  binding_ = Binding{
      .binding = fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                  std::move(request->req), this,
                                  [](SerialDevice* self, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_hardware_serial::Device>) {
                                    self->serial_.Enable(false);
                                    std::optional opt = std::exchange(self->binding_, {});
                                    ZX_ASSERT(opt.has_value());
                                    Binding& binding = opt.value();
                                    if (binding.unbind_txn.has_value()) {
                                      binding.unbind_txn.value().Reply();
                                    }
                                  }),
  };
}

void SerialDevice::GetClass(GetClassCompleter::Sync& completer) {
  completer.Reply(static_cast<fuchsia_hardware_serial::wire::Class>(serial_class_));
}

void SerialDevice::SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) {
  using fuchsia_hardware_serial::wire::CharacterWidth;
  using fuchsia_hardware_serial::wire::FlowControl;
  using fuchsia_hardware_serial::wire::Parity;
  using fuchsia_hardware_serial::wire::StopWidth;
  uint32_t flags = 0;
  switch (request->config.character_width) {
    case CharacterWidth::kBits5:
      flags |= SERIAL_DATA_BITS_5;
      break;
    case CharacterWidth::kBits6:
      flags |= SERIAL_DATA_BITS_6;
      break;
    case CharacterWidth::kBits7:
      flags |= SERIAL_DATA_BITS_7;
      break;
    case CharacterWidth::kBits8:
      flags |= SERIAL_DATA_BITS_8;
      break;
  }

  switch (request->config.stop_width) {
    case StopWidth::kBits1:
      flags |= SERIAL_STOP_BITS_1;
      break;
    case StopWidth::kBits2:
      flags |= SERIAL_STOP_BITS_2;
      break;
  }

  switch (request->config.parity) {
    case Parity::kNone:
      flags |= SERIAL_PARITY_NONE;
      break;
    case Parity::kEven:
      flags |= SERIAL_PARITY_EVEN;
      break;
    case Parity::kOdd:
      flags |= SERIAL_PARITY_ODD;
      break;
  }

  switch (request->config.control_flow) {
    case FlowControl::kNone:
      flags |= SERIAL_FLOW_CTRL_NONE;
      break;
    case FlowControl::kCtsRts:
      flags |= SERIAL_FLOW_CTRL_CTS_RTS;
      break;
  }

  zx_status_t status = SerialConfig(request->config.baud_rate, flags);
  completer.Reply(status);
}

void SerialDevice::DdkUnbind(ddk::UnbindTxn txn) {
  if (binding_.has_value()) {
    Binding& binding = binding_.value();
    ZX_ASSERT(!binding.unbind_txn.has_value());
    binding.unbind_txn.emplace(std::move(txn));
    binding.binding.Unbind();
  } else {
    txn.Reply();
  }
}

void SerialDevice::DdkRelease() {
  serial_.Enable(false);
  delete this;
}

zx_status_t SerialDevice::Create(void* ctx, zx_device_t* dev) {
  fbl::AllocChecker ac;
  std::unique_ptr<SerialDevice> sdev(new (&ac) SerialDevice(dev));

  if (!ac.check()) {
    zxlogf(ERROR, "SerialDevice::Create: no memory to allocate serial device!");
    return ZX_ERR_NO_MEMORY;
  }

  if (zx_status_t status = sdev->Init(); status != ZX_OK) {
    return status;
  }

  if (zx_status_t status = sdev->Bind(); status != ZX_OK) {
    zxlogf(ERROR, "SerialDevice::Create: Bind failed");
    sdev.release()->DdkRelease();
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* dummy = sdev.release();

  return ZX_OK;
}

zx_status_t SerialDevice::Init() {
  if (!serial_.is_valid()) {
    zxlogf(ERROR, "SerialDevice::Init: ZX_PROTOCOL_SERIAL_IMPL not available");
    return ZX_ERR_NOT_SUPPORTED;
  }

  serial_port_info_t info;
  zx_status_t status = serial_.GetInfo(&info);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SerialDevice::Init: SerialImpl::GetInfo failed %d", status);
    return status;
  }
  serial_class_ = info.serial_class;

  return ZX_OK;
}

zx_status_t SerialDevice::Bind() {
  zx_device_prop_t props[] = {
      {BIND_PROTOCOL, 0, ZX_PROTOCOL_SERIAL},
      {BIND_SERIAL_CLASS, 0, serial_class_},
  };

  return DdkAdd(ddk::DeviceAddArgs("serial").set_props(props));
}

static constexpr zx_driver_ops_t serial_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = SerialDevice::Create;
  return ops;
}();

}  // namespace serial

ZIRCON_DRIVER(serial, serial::serial_driver_ops, "zircon", "0.1");

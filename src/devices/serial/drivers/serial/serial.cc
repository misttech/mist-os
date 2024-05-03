// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial.h"

#include <fidl/fuchsia.hardware.serial/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <memory>

#include <fbl/alloc_checker.h>

namespace serial {

void SerialDevice::Read(ReadCompleter::Sync& completer) {
  fdf::Arena arena('SERI');
  serial_.buffer(arena)->Read().Then([completer = completer.ToAsync()](auto& result) mutable {
    if (!result.ok()) {
      completer.ReplyError(result.status());
    } else if (result->is_error()) {
      completer.ReplyError(result->error_value());
    } else {
      completer.ReplySuccess(result->value()->data);
    }
  });
}

void SerialDevice::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  fdf::Arena arena('SERI');
  serial_.buffer(arena)
      ->Write(request->data)
      .Then([completer = completer.ToAsync()](auto& result) mutable {
        if (!result.ok()) {
          completer.ReplyError(result.status());
        } else if (result->is_error()) {
          completer.ReplyError(result->error_value());
        } else {
          completer.ReplySuccess();
        }
      });
}

void SerialDevice::GetChannel(GetChannelRequestView request, GetChannelCompleter::Sync& completer) {
  if (binding_.has_value()) {
    request->req.Close(ZX_ERR_ALREADY_BOUND);
    return;
  }

  if (zx_status_t status = Enable(true); status != ZX_OK) {
    request->req.Close(status);
    return;
  }

  binding_ = Binding{
      .binding = fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                  std::move(request->req), this,
                                  [](SerialDevice* self, fidl::UnbindInfo,
                                     fidl::ServerEnd<fuchsia_hardware_serial::Device>) {
                                    self->Enable(false);
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
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits5;
      break;
    case CharacterWidth::kBits6:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits6;
      break;
    case CharacterWidth::kBits7:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits7;
      break;
    case CharacterWidth::kBits8:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits8;
      break;
  }

  switch (request->config.stop_width) {
    case StopWidth::kBits1:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialStopBits1;
      break;
    case StopWidth::kBits2:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialStopBits2;
      break;
  }

  switch (request->config.parity) {
    case Parity::kNone:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialParityNone;
      break;
    case Parity::kEven:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialParityEven;
      break;
    case Parity::kOdd:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialParityOdd;
      break;
  }

  switch (request->config.control_flow) {
    case FlowControl::kNone:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialFlowCtrlNone;
      break;
    case FlowControl::kCtsRts:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialFlowCtrlCtsRts;
      break;
  }

  fdf::Arena arena('SERI');
  serial_.buffer(arena)
      ->Config(request->config.baud_rate, flags)
      .Then([completer = completer.ToAsync()](auto& result) mutable {
        if (result.ok()) {
          completer.Reply(result->is_error() ? result->error_value() : ZX_OK);
        } else {
          completer.Reply(result.status());
        }
      });
}

zx_status_t SerialDevice::Enable(bool enable) {
  fdf::Arena arena('SERI');
  auto result = serial_.sync().buffer(arena)->Enable(enable);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
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
  Enable(false);
  delete this;
}

zx_status_t SerialDevice::Create(void* ctx, zx_device_t* dev) {
  zx::result serial_client =
      DdkConnectRuntimeProtocol<fuchsia_hardware_serialimpl::Service::Device>(dev);
  if (serial_client.is_error()) {
    zxlogf(ERROR, "Failed to FIDL serial client: %s", serial_client.status_string());
    return serial_client.error_value();
  }

  fbl::AllocChecker ac;
  std::unique_ptr<SerialDevice> sdev(new (&ac) SerialDevice(dev, *std::move(serial_client)));

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
  zx_status_t status = ZX_OK;
  fdf::Arena arena('SERI');
  if (auto result = serial_.sync().buffer(arena)->GetInfo(); !result.ok()) {
    status = result.status();
  } else if (result->is_error()) {
    status = result->error_value();
  } else {
    serial_class_ = static_cast<uint8_t>(result->value()->info.serial_class);
  }

  if (status != ZX_OK) {
    zxlogf(ERROR, "SerialDevice::Init: SerialImpl::GetInfo failed %d", status);
  }

  return status;
}

zx_status_t SerialDevice::Bind() {
  zx_device_prop_t props[] = {
      {BIND_PROTOCOL, 0, ZX_PROTOCOL_SERIAL},
      {BIND_SERIAL_CLASS, 0, serial_class_},
  };

  // Set proto_id so that a devfs entry is created for us.
  return DdkAdd(ddk::DeviceAddArgs("serial").set_props(props).set_proto_id(ZX_PROTOCOL_SERIAL));
}

static constexpr zx_driver_ops_t serial_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = SerialDevice::Create;
  return ops;
}();

}  // namespace serial

ZIRCON_DRIVER(serial, serial::serial_driver_ops, "zircon", "0.1");

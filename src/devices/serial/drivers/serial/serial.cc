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
  if (std::holds_alternative<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)) {
    fdf::Arena arena('SERI');
    std::get<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)
        .buffer(arena)
        ->Read()
        .Then([completer = completer.ToAsync()](auto& result) mutable {
          if (!result.ok()) {
            completer.ReplyError(result.status());
          } else if (result->is_error()) {
            completer.ReplyError(result->error_value());
          } else {
            completer.ReplySuccess(result->value()->data);
          }
        });
    return;
  }

  uint8_t data[fuchsia_io::wire::kMaxBuf];
  size_t actual;
  zx_status_t status =
      std::get<ddk::SerialImplProtocolClient>(serial_).Read(data, sizeof(data), &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(data, actual));
}

void SerialDevice::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  if (std::holds_alternative<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)) {
    fdf::Arena arena('SERI');
    std::get<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)
        .buffer(arena)
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
    return;
  }

  cpp20::span data = request->data.get();
  while (!data.empty()) {
    size_t actual;
    zx_status_t status = std::get<ddk::SerialImplProtocolClient>(serial_).Write(
        data.data(), data.size_bytes(), &actual);
    if (status != ZX_OK) {
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

  if (std::holds_alternative<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)) {
    fdf::Arena arena('SERI');
    std::get<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)
        .buffer(arena)
        ->Config(request->config.baud_rate, flags)
        .Then([completer = completer.ToAsync()](auto& result) mutable {
          if (result.ok()) {
            completer.Reply(result->is_error() ? result->error_value() : ZX_OK);
          } else {
            completer.Reply(result.status());
          }
        });
    return;
  }

  completer.Reply(
      std::get<ddk::SerialImplProtocolClient>(serial_).Config(request->config.baud_rate, flags));
}

zx_status_t SerialDevice::Enable(bool enable) {
  if (std::holds_alternative<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)) {
    fdf::Arena arena('SERI');
    auto result = std::get<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)
                      .sync()
                      .buffer(arena)
                      ->Enable(enable);
    if (!result.ok()) {
      return result.status();
    }
    if (result->is_error()) {
      return result->error_value();
    }
    return ZX_OK;
  }

  return std::get<ddk::SerialImplProtocolClient>(serial_).Enable(enable);
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
  SerialType serial(ddk::SerialImplProtocolClient{dev});
  if (!std::get<ddk::SerialImplProtocolClient>(serial).is_valid()) {
    zx::result serial_client =
        DdkConnectRuntimeProtocol<fuchsia_hardware_serialimpl::Service::Device>(dev);
    if (serial_client.is_error()) {
      zxlogf(ERROR, "Failed to get Banjo or FIDL serial client: %s", serial_client.status_string());
      return serial_client.error_value();
    }

    serial.emplace<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(
        *std::move(serial_client), fdf::Dispatcher::GetCurrent()->get());
  }

  fbl::AllocChecker ac;
  std::unique_ptr<SerialDevice> sdev(new (&ac) SerialDevice(dev, std::move(serial)));

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
  if (std::holds_alternative<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)) {
    fdf::Arena arena('SERI');
    auto result = std::get<fdf::WireClient<fuchsia_hardware_serialimpl::Device>>(serial_)
                      .sync()
                      .buffer(arena)
                      ->GetInfo();
    if (!result.ok()) {
      status = result.status();
    } else if (result->is_error()) {
      status = result->error_value();
    } else {
      serial_class_ = static_cast<uint8_t>(result->value()->info.serial_class);
    }
  } else if (std::holds_alternative<ddk::SerialImplProtocolClient>(serial_)) {
    if (!std::get<ddk::SerialImplProtocolClient>(serial_).is_valid()) {
      zxlogf(ERROR, "SerialDevice::Init: ZX_PROTOCOL_SERIAL_IMPL not available");
      return ZX_ERR_NOT_SUPPORTED;
    }

    serial_port_info_t info;
    status = std::get<ddk::SerialImplProtocolClient>(serial_).GetInfo(&info);
    if (status == ZX_OK) {
      serial_class_ = info.serial_class;
    }
  }

  if (status != ZX_OK) {
    zxlogf(ERROR, "SerialDevice::Init: SerialImpl::GetInfo failed %d", status);
    return status;
  }

  return ZX_OK;
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

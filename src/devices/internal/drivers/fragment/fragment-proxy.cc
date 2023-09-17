// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/internal/drivers/fragment/fragment-proxy.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>
#include <lib/sync/completion.h>

#include <memory>

namespace fragment {

zx_status_t FragmentProxy::Create(void* ctx, zx_device_t* parent) {
  zx::channel client, server;
  if (zx_status_t status = zx::channel::create(0, &client, &server); status != ZX_OK) {
    zxlogf(ERROR, "Failed to create endpoints: %s", zx_status_get_string(status));
    return ZX_ERR_INTERNAL;
  }

  zx_status_t status = device_connect_fidl_protocol(parent, "proxy_channel", server.release());
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to connect fidl protocol: %s", zx_status_get_string(status));
    return status;
  }

  auto dev = std::make_unique<FragmentProxy>(parent, std::move(client));
  status = dev->DdkAdd("fragment-proxy", DEVICE_ADD_NON_BINDABLE);
  if (status == ZX_OK) {
    // devmgr owns the memory now
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

zx_status_t FragmentProxy::DdkGetProtocol(uint32_t proto_id, void* out) {
  auto* proto = static_cast<ddk::AnyProtocol*>(out);
  proto->ctx = this;

  switch (proto_id) {
    case ZX_PROTOCOL_DAI:
      proto->ops = &dai_protocol_ops_;
      return ZX_OK;
    case ZX_PROTOCOL_GPIO:
      proto->ops = &gpio_protocol_ops_;
      return ZX_OK;
    case ZX_PROTOCOL_PDEV:
      proto->ops = &pdev_protocol_ops_;
      return ZX_OK;
    case ZX_PROTOCOL_SPI:
      proto->ops = &spi_protocol_ops_;
      return ZX_OK;
    case ZX_PROTOCOL_SYSMEM:
      proto->ops = &sysmem_protocol_ops_;
      return ZX_OK;
    default:
      zxlogf(ERROR, "%s unsupported protocol \'%u\'", __func__, proto_id);
      return ZX_ERR_NOT_SUPPORTED;
  }
}

void FragmentProxy::DdkRelease() { delete this; }

zx_status_t FragmentProxy::Rpc(const ProxyRequest* req, size_t req_length, ProxyResponse* resp,
                               size_t resp_length, const zx_handle_t* in_handles,
                               size_t in_handle_count, zx_handle_t* out_handles,
                               size_t out_handle_count, size_t* out_actual) {
  uint32_t resp_size, handle_count;

  zx_channel_call_args_t args = {
      .wr_bytes = req,
      .wr_handles = in_handles,
      .rd_bytes = resp,
      .rd_handles = out_handles,
      .wr_num_bytes = static_cast<uint32_t>(req_length),
      .wr_num_handles = static_cast<uint32_t>(in_handle_count),
      .rd_num_bytes = static_cast<uint32_t>(resp_length),
      .rd_num_handles = static_cast<uint32_t>(out_handle_count),
  };
  auto status = rpc_.call(0, zx::time::infinite(), &args, &resp_size, &handle_count);
  if (status != ZX_OK) {
    return status;
  }

  status = resp->status;

  if (status == ZX_OK && resp_size < sizeof(*resp)) {
    zxlogf(ERROR, "PlatformProxy::Rpc resp_size too short: %u", resp_size);
    status = ZX_ERR_INTERNAL;
    goto fail;
  } else if (status == ZX_OK && handle_count != out_handle_count) {
    zxlogf(ERROR, "PlatformProxy::Rpc handle count %u expected %zu", handle_count,
           out_handle_count);
    status = ZX_ERR_INTERNAL;
    goto fail;
  }

  if (out_actual) {
    *out_actual = resp_size;
  }

fail:
  if (status != ZX_OK) {
    for (uint32_t i = 0; i < handle_count; i++) {
      zx_handle_close(out_handles[i]);
    }
  }
  return status;
}

zx_status_t FragmentProxy::DaiConnect(zx::channel chan) {
  DaiProxyRequest req = {};
  DaiProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_DAI;
  req.op = DaiOp::GET_CHANNEL;
  zx_handle_t handle = chan.release();

  auto status =
      Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp), &handle, 1, nullptr, 0, nullptr);
  if (status != ZX_OK) {
    return status;
  }
  return ZX_OK;
}

zx_status_t FragmentProxy::GpioConfigIn(uint32_t flags) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::CONFIG_IN;
  req.flags = flags;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
}

zx_status_t FragmentProxy::GpioConfigOut(uint8_t initial_value) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::CONFIG_OUT;
  req.value = initial_value;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
}

zx_status_t FragmentProxy::GpioSetAltFunction(uint64_t function) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::SET_ALT_FUNCTION;
  req.alt_function = function;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
}

zx_status_t FragmentProxy::GpioGetInterrupt(uint32_t flags, zx::interrupt* out_irq) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::GET_INTERRUPT;
  req.flags = flags;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp), nullptr, 0,
             out_irq->reset_and_get_address(), 1, nullptr);
}

zx_status_t FragmentProxy::GpioSetPolarity(uint32_t polarity) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::SET_POLARITY;
  req.polarity = polarity;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
}

zx_status_t FragmentProxy::GpioGetDriveStrength(uint64_t* ds_ua) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::GET_DRIVE_STRENGTH;

  ZX_ASSERT(ds_ua != nullptr);

  auto status = Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
  if (status == ZX_OK) {
    *ds_ua = resp.out_actual_ds_ua;
  }
  return status;
}

zx_status_t FragmentProxy::GpioSetDriveStrength(uint64_t ds_ua, uint64_t* out_actual_ds_ua) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::SET_DRIVE_STRENGTH;
  req.ds_ua = ds_ua;

  auto status = Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
  if ((status == ZX_OK) && out_actual_ds_ua) {
    *out_actual_ds_ua = resp.out_actual_ds_ua;
  }
  return status;
}

zx_status_t FragmentProxy::GpioReleaseInterrupt() {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::RELEASE_INTERRUPT;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
}

zx_status_t FragmentProxy::GpioRead(uint8_t* out_value) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::READ;

  auto status = Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));

  if (status != ZX_OK) {
    return status;
  }
  *out_value = resp.value;
  return ZX_OK;
}

zx_status_t FragmentProxy::GpioWrite(uint8_t value) {
  GpioProxyRequest req = {};
  GpioProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_GPIO;
  req.op = GpioOp::WRITE;
  req.value = value;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
}

zx_status_t FragmentProxy::PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio) {
  PdevProxyRequest req = {};
  PdevProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_PDEV;
  req.op = PdevOp::GET_MMIO;
  req.index = index;

  auto status = Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp), nullptr, 0,
                    &out_mmio->vmo, 1, nullptr);
  if (status == ZX_OK) {
    out_mmio->offset = resp.offset;
    out_mmio->size = resp.size;
  }
  return status;
}

zx_status_t FragmentProxy::PDevGetInterrupt(uint32_t index, uint32_t flags,
                                            zx::interrupt* out_irq) {
  PdevProxyRequest req = {};
  PdevProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_PDEV;
  req.op = PdevOp::GET_INTERRUPT;
  req.index = index;
  req.flags = flags;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp), nullptr, 0,
             out_irq->reset_and_get_address(), 1, nullptr);
}

zx_status_t FragmentProxy::PDevGetBti(uint32_t index, zx::bti* out_bti) {
  PdevProxyRequest req = {};
  PdevProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_PDEV;
  req.op = PdevOp::GET_BTI;
  req.index = index;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp), nullptr, 0,
             out_bti->reset_and_get_address(), 1, nullptr);
}

zx_status_t FragmentProxy::PDevGetSmc(uint32_t index, zx::resource* out_resource) {
  PdevProxyRequest req = {};
  PdevProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_PDEV;
  req.op = PdevOp::GET_SMC;
  req.index = index;

  return Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp), nullptr, 0,
             out_resource->reset_and_get_address(), 1, nullptr);
}

zx_status_t FragmentProxy::PDevGetDeviceInfo(pdev_device_info_t* out_info) {
  PdevProxyRequest req = {};
  PdevProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_PDEV;
  req.op = PdevOp::GET_DEVICE_INFO;

  auto status = Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
  if (status != ZX_OK) {
    return status;
  }
  memcpy(out_info, &resp.device_info, sizeof(*out_info));
  return ZX_OK;
}

zx_status_t FragmentProxy::PDevGetBoardInfo(pdev_board_info_t* out_info) {
  PdevProxyRequest req = {};
  PdevProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_PDEV;
  req.op = PdevOp::GET_BOARD_INFO;

  auto status = Rpc(&req.header, sizeof(req), &resp.header, sizeof(resp));
  if (status != ZX_OK) {
    return status;
  }
  memcpy(out_info, &resp.board_info, sizeof(*out_info));
  return ZX_OK;
}

zx_status_t FragmentProxy::PDevDeviceAdd(uint32_t index, const device_add_args_t* args,
                                         zx_device_t** device) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t FragmentProxy::PDevGetProtocol(uint32_t proto_id, uint32_t index, void* out_protocol,
                                           size_t protocol_size, size_t* protocol_actual) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t FragmentProxy::SpiTransmit(const uint8_t* txdata_list, size_t txdata_count) {
  return SpiExchange(txdata_list, txdata_count, NULL, 0, NULL);
}

zx_status_t FragmentProxy::SpiReceive(uint32_t size, uint8_t* out_rxdata_list, size_t rxdata_count,
                                      size_t* out_rxdata_actual) {
  return SpiExchange(NULL, 0, out_rxdata_list, size, out_rxdata_actual);
}
zx_status_t FragmentProxy::SpiExchange(const uint8_t* txdata_list, size_t txdata_count,
                                       uint8_t* out_rxdata_list, size_t rxdata_count,
                                       size_t* out_rxdata_actual) {
  uint8_t req_buffer[kProxyMaxTransferSize];
  auto req = reinterpret_cast<SpiProxyRequest*>(req_buffer);
  req->header.proto_id = ZX_PROTOCOL_SPI;

  if (txdata_count && rxdata_count) {
    req->op = SpiOp::EXCHANGE;
    req->length = txdata_count;
  } else if (txdata_count) {
    req->op = SpiOp::TRANSMIT;
    req->length = txdata_count;
  } else {
    req->op = SpiOp::RECEIVE;
    req->length = rxdata_count;
  }

  size_t req_length;
  if (__builtin_add_overflow(sizeof(SpiProxyRequest), txdata_count, &req_length) ||
      req_length >= kProxyMaxTransferSize) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  size_t resp_length;
  if (__builtin_add_overflow(sizeof(SpiProxyResponse), rxdata_count, &resp_length) ||
      resp_length >= kProxyMaxTransferSize) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  if (txdata_count) {
    uint8_t* p_write = reinterpret_cast<uint8_t*>(&req[1]);
    memcpy(p_write, txdata_list, txdata_count);
  }

  uint8_t resp_buffer[kProxyMaxTransferSize];
  auto resp = reinterpret_cast<SpiProxyResponse*>(resp_buffer);

  size_t actual;
  auto status = Rpc(&req->header, static_cast<uint32_t>(req_length), &resp->header,
                    static_cast<uint32_t>(resp_length), nullptr, 0, nullptr, 0, &actual);
  if (status != ZX_OK) {
    return status;
  }

  if (actual != resp_length) {
    return ZX_ERR_INTERNAL;
  }

  if (rxdata_count) {
    uint8_t* p_read = reinterpret_cast<uint8_t*>(&resp[1]);
    memcpy(out_rxdata_list, p_read, rxdata_count);
    *out_rxdata_actual = rxdata_count;
  }

  return ZX_OK;
}

zx_status_t FragmentProxy::SysmemConnect(zx::channel allocator2_request) {
  SysmemProxyRequest req = {};
  ProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_SYSMEM;
  req.op = SysmemOp::CONNECT;
  zx_handle_t handle = allocator2_request.release();

  return Rpc(&req.header, sizeof(req), &resp, sizeof(resp), &handle, 1, nullptr, 0, nullptr);
}

zx_status_t FragmentProxy::SysmemRegisterHeap(uint64_t heap, zx::channel heap_connection) {
  SysmemProxyRequest req = {};
  ProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_SYSMEM;
  req.op = SysmemOp::REGISTER_HEAP;
  req.heap = heap;
  zx_handle_t handle = heap_connection.release();

  return Rpc(&req.header, sizeof(req), &resp, sizeof(resp), &handle, 1, nullptr, 0, nullptr);
}

zx_status_t FragmentProxy::SysmemRegisterSecureMem(zx::channel secure_mem_connection) {
  SysmemProxyRequest req = {};
  ProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_SYSMEM;
  req.op = SysmemOp::REGISTER_SECURE_MEM;
  zx_handle_t handle = secure_mem_connection.release();

  return Rpc(&req.header, sizeof(req), &resp, sizeof(resp), &handle, 1, nullptr, 0, nullptr);
}

zx_status_t FragmentProxy::SysmemUnregisterSecureMem() {
  SysmemProxyRequest req = {};
  ProxyResponse resp = {};
  req.header.proto_id = ZX_PROTOCOL_SYSMEM;
  req.op = SysmemOp::UNREGISTER_SECURE_MEM;

  return Rpc(&req.header, sizeof(req), &resp, sizeof(resp), nullptr, 0, nullptr, 0, nullptr);
}

const zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = FragmentProxy::Create;
  return ops;
}();

}  // namespace fragment

ZIRCON_DRIVER(fragment_proxy, fragment::driver_ops, "zircon", "0.1");

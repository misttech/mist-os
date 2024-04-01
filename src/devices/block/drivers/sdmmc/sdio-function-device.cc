// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdio-function-device.h"

#include <lib/ddk/binding_driver.h>
#include <lib/driver/compat/cpp/compat.h>

#include <fbl/alloc_checker.h>

#include "sdio-controller-device.h"
#include "sdmmc-root-device.h"

namespace sdmmc {

using fuchsia_hardware_sdio::wire::SdioHwInfo;

zx_status_t SdioFunctionDevice::Create(SdioControllerDevice* sdio_parent, uint32_t func,
                                       std::unique_ptr<SdioFunctionDevice>* out_dev) {
  fbl::AllocChecker ac;
  out_dev->reset(new (&ac) SdioFunctionDevice(sdio_parent, func));
  if (!ac.check()) {
    FDF_LOGL(ERROR, sdio_parent->logger(), "failed to allocate device memory");
    return ZX_ERR_NO_MEMORY;
  }

  return ZX_OK;
}

zx_status_t SdioFunctionDevice::AddDevice(const sdio_func_hw_info_t& hw_info) {
  {
    const std::string path_from_parent = std::string(sdio_parent_->parent()->driver_name()) + "/" +
                                         std::string(sdio_parent_->kDeviceName) + "/";
    compat::DeviceServer::BanjoConfig banjo_config;
    banjo_config.callbacks[ZX_PROTOCOL_SDIO] = sdio_server_.callback();

    auto result = compat_server_.Initialize(
        sdio_parent_->parent()->driver_incoming(), sdio_parent_->parent()->driver_outgoing(),
        sdio_parent_->parent()->driver_node_name(), sdio_function_name_,
        compat::ForwardMetadata::None(), std::move(banjo_config), path_from_parent);
    if (result.is_error()) {
      return result.status_value();
    }
  }

  {
    fuchsia_hardware_sdio::Service::InstanceHandler handler({
        .device = fit::bind_member<&SdioFunctionDevice::Serve>(this),
    });
    auto result =
        sdio_parent_->parent()->driver_outgoing()->AddService<fuchsia_hardware_sdio::Service>(
            std::move(handler), sdio_function_name_);
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add SDIO service: %s", result.status_string());
      return result.status_value();
    }
  }

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 4);
  properties[0] = fdf::MakeProperty(arena, BIND_PROTOCOL, ZX_PROTOCOL_SDIO);
  properties[1] = fdf::MakeProperty(arena, BIND_SDIO_VID, hw_info.manufacturer_id);
  properties[2] = fdf::MakeProperty(arena, BIND_SDIO_PID, hw_info.product_id);
  properties[3] = fdf::MakeProperty(arena, BIND_SDIO_FUNCTION, function_);

  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_sdio::Service>(arena, sdio_function_name_));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, sdio_function_name_)
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result =
      sdio_parent_->sdio_controller_node()->AddChild(args, std::move(controller_server_end), {});
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child sdio function device: %s",
             result.status_string());
    return result.status();
  }

  return ZX_OK;
}

void SdioFunctionDevice::Serve(fidl::ServerEnd<fuchsia_hardware_sdio::Device> request) {
  fidl::BindServer(sdio_parent_->parent()->driver_async_dispatcher(), std::move(request), this);
}

zx_status_t SdioFunctionDevice::SdioGetDevHwInfo(sdio_hw_info_t* out_hw_info) {
  return sdio_parent_->SdioGetDevHwInfo(function_, out_hw_info);
}

zx_status_t SdioFunctionDevice::SdioEnableFn() { return sdio_parent_->SdioEnableFn(function_); }

zx_status_t SdioFunctionDevice::SdioDisableFn() { return sdio_parent_->SdioDisableFn(function_); }

zx_status_t SdioFunctionDevice::SdioEnableFnIntr() {
  return sdio_parent_->SdioEnableFnIntr(function_);
}

zx_status_t SdioFunctionDevice::SdioDisableFnIntr() {
  return sdio_parent_->SdioDisableFnIntr(function_);
}

zx_status_t SdioFunctionDevice::SdioUpdateBlockSize(uint16_t blk_sz, bool deflt) {
  return sdio_parent_->SdioUpdateBlockSize(function_, blk_sz, deflt);
}

zx_status_t SdioFunctionDevice::SdioGetBlockSize(uint16_t* out_cur_blk_size) {
  return sdio_parent_->SdioGetBlockSize(function_, out_cur_blk_size);
}

zx_status_t SdioFunctionDevice::SdioDoRwByte(bool write, uint32_t addr, uint8_t write_byte,
                                             uint8_t* out_read_byte) {
  return sdio_parent_->SdioDoRwByte(write, function_, addr, write_byte, out_read_byte);
}

zx_status_t SdioFunctionDevice::SdioGetInBandIntr(zx::interrupt* out_irq) {
  return sdio_parent_->SdioGetInBandIntr(function_, out_irq);
}

void SdioFunctionDevice::SdioAckInBandIntr() { return sdio_parent_->SdioAckInBandIntr(function_); }

zx_status_t SdioFunctionDevice::SdioIoAbort() { return sdio_parent_->SdioIoAbort(function_); }

zx_status_t SdioFunctionDevice::SdioIntrPending(bool* out_pending) {
  return sdio_parent_->SdioIntrPending(function_, out_pending);
}

zx_status_t SdioFunctionDevice::SdioDoVendorControlRwByte(bool write, uint8_t addr,
                                                          uint8_t write_byte,
                                                          uint8_t* out_read_byte) {
  return sdio_parent_->SdioDoVendorControlRwByte(write, addr, write_byte, out_read_byte);
}

void SdioFunctionDevice::GetDevHwInfo(GetDevHwInfoCompleter::Sync& completer) {
  sdio_hw_info_t hw_info = {};
  SdioHwInfo fidl_hw_info = {};
  zx_status_t status = SdioGetDevHwInfo(&hw_info);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  static_assert(sizeof(fidl_hw_info) == sizeof(hw_info));
  memcpy(&fidl_hw_info, &hw_info, sizeof(fidl_hw_info));
  completer.ReplySuccess(fidl_hw_info);
}

void SdioFunctionDevice::EnableFn(EnableFnCompleter::Sync& completer) {
  zx_status_t status = SdioEnableFn();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::DisableFn(DisableFnCompleter::Sync& completer) {
  zx_status_t status = SdioDisableFn();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::EnableFnIntr(EnableFnIntrCompleter::Sync& completer) {
  zx_status_t status = SdioEnableFnIntr();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::DisableFnIntr(DisableFnIntrCompleter::Sync& completer) {
  zx_status_t status = SdioDisableFnIntr();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::UpdateBlockSize(UpdateBlockSizeRequestView request,
                                         UpdateBlockSizeCompleter::Sync& completer) {
  zx_status_t status = SdioUpdateBlockSize(request->blk_sz, request->deflt);
  if (status != ZX_OK)
    completer.ReplyError(status);
  else
    completer.ReplySuccess();
}

void SdioFunctionDevice::GetBlockSize(GetBlockSizeCompleter::Sync& completer) {
  uint16_t cur_blk_size;
  zx_status_t status = SdioGetBlockSize(&cur_blk_size);
  if (status != ZX_OK)
    completer.ReplyError(status);
  else
    completer.ReplySuccess(cur_blk_size);
}

void SdioFunctionDevice::DoRwTxn(DoRwTxnRequestView request, DoRwTxnCompleter::Sync& completer) {
  const SdioControllerDevice::SdioRwTxn<fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion> txn{
      .addr = request->txn.addr,
      .incr = request->txn.incr,
      .write = request->txn.write,
      .buffers = {request->txn.buffers.data(), request->txn.buffers.count()},
  };

  if (zx_status_t status = sdio_parent_->SdioDoRwTxn(function_, txn); status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void SdioFunctionDevice::DoRwByte(DoRwByteRequestView request, DoRwByteCompleter::Sync& completer) {
  zx_status_t status =
      SdioDoRwByte(request->write, request->addr, request->write_byte, &request->write_byte);
  if (status != ZX_OK)
    completer.ReplyError(status);
  else
    completer.ReplySuccess(request->write_byte);
}

void SdioFunctionDevice::GetInBandIntr(GetInBandIntrCompleter::Sync& completer) {
  zx::interrupt irq;
  zx_status_t status = SdioGetInBandIntr(&irq);
  if (status != ZX_OK)
    completer.ReplyError(status);
  else
    completer.ReplySuccess(std::move(irq));
}

void SdioFunctionDevice::IoAbort(IoAbortCompleter::Sync& completer) {
  zx_status_t status = SdioIoAbort();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::IntrPending(IntrPendingCompleter::Sync& completer) {
  bool pending;
  zx_status_t status = SdioIntrPending(&pending);
  if (status == ZX_OK)
    completer.ReplySuccess(pending);
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::DoVendorControlRwByte(DoVendorControlRwByteRequestView request,
                                               DoVendorControlRwByteCompleter::Sync& completer) {
  zx_status_t status = SdioDoVendorControlRwByte(request->write, request->addr, request->write_byte,
                                                 &request->write_byte);
  if (status == ZX_OK)
    completer.ReplySuccess(request->write_byte);
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::AckInBandIntr(AckInBandIntrCompleter::Sync& completer) {
  SdioAckInBandIntr();
}

void SdioFunctionDevice::RegisterVmo(RegisterVmoRequestView request,
                                     RegisterVmoCompleter::Sync& completer) {
  zx_status_t status = SdioRegisterVmo(request->vmo_id, std::move(request->vmo), request->offset,
                                       request->size, request->vmo_rights);
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::UnregisterVmo(UnregisterVmoRequestView request,
                                       UnregisterVmoCompleter::Sync& completer) {
  zx::vmo out_vmo;

  zx_status_t status = SdioUnregisterVmo(request->vmo_id, &out_vmo);
  if (status != ZX_OK)
    completer.ReplyError(status);
  else
    completer.ReplySuccess(std::move(out_vmo));
}

void SdioFunctionDevice::RequestCardReset(RequestCardResetCompleter::Sync& completer) {
  zx_status_t status = SdioRequestCardReset();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

void SdioFunctionDevice::PerformTuning(PerformTuningCompleter::Sync& completer) {
  zx_status_t status = SdioPerformTuning();
  if (status == ZX_OK)
    completer.ReplySuccess();
  else
    completer.ReplyError(status);
}

zx_status_t SdioFunctionDevice::SdioRegisterVmo(uint32_t vmo_id, zx::vmo vmo, uint64_t offset,
                                                uint64_t size, uint32_t vmo_rights) {
  return sdio_parent_->SdioRegisterVmo(function_, vmo_id, std::move(vmo), offset, size, vmo_rights);
}

zx_status_t SdioFunctionDevice::SdioUnregisterVmo(uint32_t vmo_id, zx::vmo* out_vmo) {
  return sdio_parent_->SdioUnregisterVmo(function_, vmo_id, out_vmo);
}

zx_status_t SdioFunctionDevice::SdioDoRwTxn(const sdio_rw_txn_t* txn) {
  return sdio_parent_->SdioDoRwTxn(function_, txn);
}

zx_status_t SdioFunctionDevice::SdioRequestCardReset() {
  return sdio_parent_->SdioRequestCardReset();
}

zx_status_t SdioFunctionDevice::SdioPerformTuning() { return sdio_parent_->SdioPerformTuning(); }

fdf::Logger& SdioFunctionDevice::logger() { return sdio_parent_->logger(); }

}  // namespace sdmmc

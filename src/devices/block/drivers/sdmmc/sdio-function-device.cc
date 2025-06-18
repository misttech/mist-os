// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdio-function-device.h"

#include <lib/ddk/binding_driver.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/power/cpp/power-support.h>

#include <format>
#include <utility>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/sdio/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "sdio-controller-device.h"
#include "sdmmc-root-device.h"
#include "tools/power_config/lib/cpp/power_config.h"

namespace {

std::pair<zx::error<zx_status_t>, const char*> MapPowerError(fdf_power::Error error) {
  return {fdf_power::ErrorToZxError(error), fdf_power::ErrorToString(error)};
}

std::pair<zx::error<zx_status_t>, const char*> MapLeaseError(
    fuchsia_power_broker::LeaseError error) {
  switch (error) {
    case fuchsia_power_broker::LeaseError::kInternal:
      return {zx::error(ZX_ERR_INTERNAL), "Internal error"};
    case fuchsia_power_broker::LeaseError::kNotAuthorized:
      return {zx::error(ZX_ERR_ACCESS_DENIED), "Not authorized"};
    case fuchsia_power_broker::LeaseError::kInvalidLevel:
      return {zx::error(ZX_ERR_INVALID_ARGS), "Invalid level"};
    default:
      return {zx::error(ZX_ERR_INTERNAL), "(unknown)"};
  }
}

}  // namespace

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
    banjo_config.callbacks[bind_fuchsia_sdio::BIND_PROTOCOL_DEVICE] = sdio_server_.callback();

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
        .device =
            [this](fidl::ServerEnd<fuchsia_hardware_sdio::Device> request) {
              fidl::BindServer(sdio_parent_->parent()->driver_async_dispatcher(),
                               std::move(request), &zircon_transport_impl_);
            },
    });
    auto result =
        sdio_parent_->parent()->driver_outgoing()->AddService<fuchsia_hardware_sdio::Service>(
            std::move(handler), sdio_function_name_);
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add SDIO service: %s", result.status_string());
      return result.status_value();
    }
  }

  {
    fuchsia_hardware_sdio::DriverService::InstanceHandler handler({
        .device =
            [this](fdf::ServerEnd<fuchsia_hardware_sdio::DriverDevice> request) {
              fdf::BindServer(sdio_parent_->parent()->driver_dispatcher()->get(),
                              std::move(request), &driver_transport_impl_);
            },
    });
    auto result =
        sdio_parent_->parent()->driver_outgoing()->AddService<fuchsia_hardware_sdio::DriverService>(
            std::move(handler), sdio_function_name_);
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add SDIO driver service: %s", result.status_string());
      return result.status_value();
    }
  }

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  controller_.Bind(std::move(controller_client_end));

  fidl::Arena arena;

  auto connector = devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    return connector.status_value();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(*std::move(connector))
                   .connector_supports(fuchsia_device_fs::ConnectionType::kDevice)
                   .class_name("sdio")
                   .Build();

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 4);
  properties[0] =
      fdf::MakeProperty(arena, bind_fuchsia::PROTOCOL, bind_fuchsia_sdio::BIND_PROTOCOL_DEVICE);
  properties[1] = fdf::MakeProperty(arena, bind_fuchsia::SDIO_VID, hw_info.manufacturer_id);
  properties[2] = fdf::MakeProperty(arena, bind_fuchsia::SDIO_PID, hw_info.product_id);
  properties[3] =
      fdf::MakeProperty(arena, bind_fuchsia::SDIO_FUNCTION, static_cast<uint32_t>(function_));

  std::vector<fuchsia_driver_framework::wire::Offer> offers = compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_sdio::Service>(arena, sdio_function_name_));
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_sdio::DriverService>(arena, sdio_function_name_));

  if (sdio_parent_->parent()->config().enable_suspend()) {
    // TODO(b/425459741) Treat errors as fatal again once all products are
    // wired proerly.
    if (zx::result<> result = ConfigurePowerManagement(); !result.is_error()) {
      fuchsia_hardware_power::PowerTokenService::InstanceHandler handler({
          .token_provider = power_token_provider_bindings_.CreateHandler(
              this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure),
      });
      result = sdio_parent_->parent()
                   ->driver_outgoing()
                   ->AddService<fuchsia_hardware_power::PowerTokenService>(std::move(handler),
                                                                           sdio_function_name_);
      if (result.is_error()) {
        FDF_LOGL(ERROR, logger(), "Failed to add power token service: %s", result.status_string());
        return result.status_value();
      }

      offers.push_back(
          fdf::MakeOffer2<fuchsia_hardware_power::PowerTokenService>(arena, sdio_function_name_));
    } else {
      FDF_LOGL(ERROR, logger(), "Power configuration failed, power management disabled: %s",
               result.status_string());
    }
  }

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, sdio_function_name_)
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .devfs_args(devfs)
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

void SdioFunctionDevice::DriverTransportImpl::GetDevHwInfo(fdf::Arena& arena,
                                                           GetDevHwInfoCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceGetDevHwInfoResponse response{};
  completer.buffer(arena).Reply(parent_->GetDevHwInfo(&response));
}

void SdioFunctionDevice::DriverTransportImpl::EnableFn(fdf::Arena& arena,
                                                       EnableFnCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->EnableFn());
}

void SdioFunctionDevice::DriverTransportImpl::DisableFn(fdf::Arena& arena,
                                                        DisableFnCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->DisableFn());
}

void SdioFunctionDevice::DriverTransportImpl::IoReady(fdf::Arena& arena,
                                                      IoReadyCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceIoReadyResponse response{};
  completer.buffer(arena).Reply(parent_->IoReady(&response));
}

void SdioFunctionDevice::DriverTransportImpl::EnableFnIntr(fdf::Arena& arena,
                                                           EnableFnIntrCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->EnableFnIntr());
}

void SdioFunctionDevice::DriverTransportImpl::DisableFnIntr(
    fdf::Arena& arena, DisableFnIntrCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->DisableFnIntr());
}

void SdioFunctionDevice::DriverTransportImpl::UpdateBlockSize(
    fuchsia_hardware_sdio::wire::DeviceUpdateBlockSizeRequest* request, fdf::Arena& arena,
    UpdateBlockSizeCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->UpdateBlockSize(request));
}

void SdioFunctionDevice::DriverTransportImpl::GetBlockSize(fdf::Arena& arena,
                                                           GetBlockSizeCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceGetBlockSizeResponse response{};
  completer.buffer(arena).Reply(parent_->GetBlockSize(&response));
}

void SdioFunctionDevice::DriverTransportImpl::DoRwByte(
    fuchsia_hardware_sdio::wire::DeviceDoRwByteRequest* request, fdf::Arena& arena,
    DoRwByteCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceDoRwByteResponse response{};
  completer.buffer(arena).Reply(parent_->DoRwByte(request, &response));
}

void SdioFunctionDevice::DriverTransportImpl::GetInBandIntr(
    fdf::Arena& arena, GetInBandIntrCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceGetInBandIntrResponse response{};
  completer.buffer(arena).Reply(parent_->GetInBandIntr(&response));
}

void SdioFunctionDevice::DriverTransportImpl::AckInBandIntr(
    fdf::Arena& arena, AckInBandIntrCompleter::Sync& completer) {
  parent_->AckInBandIntr();
}

void SdioFunctionDevice::DriverTransportImpl::IoAbort(fdf::Arena& arena,
                                                      IoAbortCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->IoAbort());
}

void SdioFunctionDevice::DriverTransportImpl::IntrPending(fdf::Arena& arena,
                                                          IntrPendingCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceIntrPendingResponse response{};
  completer.buffer(arena).Reply(parent_->IntrPending(&response));
}

void SdioFunctionDevice::DriverTransportImpl::DoVendorControlRwByte(
    fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteRequest* request, fdf::Arena& arena,
    DoVendorControlRwByteCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteResponse response{};
  completer.buffer(arena).Reply(parent_->DoVendorControlRwByte(request, &response));
}

void SdioFunctionDevice::DriverTransportImpl::RegisterVmo(
    fuchsia_hardware_sdio::wire::DeviceRegisterVmoRequest* request, fdf::Arena& arena,
    RegisterVmoCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->RegisterVmo(request));
}

void SdioFunctionDevice::DriverTransportImpl::UnregisterVmo(
    fuchsia_hardware_sdio::wire::DeviceUnregisterVmoRequest* request, fdf::Arena& arena,
    UnregisterVmoCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceUnregisterVmoResponse response{};
  completer.buffer(arena).Reply(parent_->UnregisterVmo(request, &response));
}

void SdioFunctionDevice::DriverTransportImpl::DoRwTxn(
    fuchsia_hardware_sdio::wire::DeviceDoRwTxnRequest* request, fdf::Arena& arena,
    DoRwTxnCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->DoRwTxn(request));
}

void SdioFunctionDevice::DriverTransportImpl::RequestCardReset(
    fdf::Arena& arena, RequestCardResetCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->RequestCardReset());
}

void SdioFunctionDevice::DriverTransportImpl::PerformTuning(
    fdf::Arena& arena, PerformTuningCompleter::Sync& completer) {
  completer.buffer(arena).Reply(parent_->PerformTuning());
}

void SdioFunctionDevice::ZirconTransportImpl::DevfsConnect(
    fidl::ServerEnd<fuchsia_hardware_sdio::Device> server) {
  bindings_.AddBinding(parent_->sdio_parent_->parent()->driver_async_dispatcher(),
                       std::move(server), this, fidl::kIgnoreBindingClosure);
}

void SdioFunctionDevice::ZirconTransportImpl::GetDevHwInfo(GetDevHwInfoCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceGetDevHwInfoResponse response{};
  completer.Reply(parent_->GetDevHwInfo(&response));
}

void SdioFunctionDevice::ZirconTransportImpl::EnableFn(EnableFnCompleter::Sync& completer) {
  completer.Reply(parent_->EnableFn());
}

void SdioFunctionDevice::ZirconTransportImpl::DisableFn(DisableFnCompleter::Sync& completer) {
  completer.Reply(parent_->DisableFn());
}

void SdioFunctionDevice::ZirconTransportImpl::IoReady(IoReadyCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceIoReadyResponse response{};
  completer.Reply(parent_->IoReady(&response));
}

void SdioFunctionDevice::ZirconTransportImpl::EnableFnIntr(EnableFnIntrCompleter::Sync& completer) {
  completer.Reply(parent_->EnableFnIntr());
}

void SdioFunctionDevice::ZirconTransportImpl::DisableFnIntr(
    DisableFnIntrCompleter::Sync& completer) {
  completer.Reply(parent_->DisableFnIntr());
}

void SdioFunctionDevice::ZirconTransportImpl::UpdateBlockSize(
    UpdateBlockSizeRequestView request, UpdateBlockSizeCompleter::Sync& completer) {
  completer.Reply(parent_->UpdateBlockSize(request));
}

void SdioFunctionDevice::ZirconTransportImpl::GetBlockSize(GetBlockSizeCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceGetBlockSizeResponse response{};
  completer.Reply(parent_->GetBlockSize(&response));
}

void SdioFunctionDevice::ZirconTransportImpl::DoRwByte(DoRwByteRequestView request,
                                                       DoRwByteCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceDoRwByteResponse response{};
  completer.Reply(parent_->DoRwByte(request, &response));
}

void SdioFunctionDevice::ZirconTransportImpl::GetInBandIntr(
    GetInBandIntrCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceGetInBandIntrResponse response{};
  completer.Reply(parent_->GetInBandIntr(&response));
}

void SdioFunctionDevice::ZirconTransportImpl::AckInBandIntr(
    AckInBandIntrCompleter::Sync& completer) {
  parent_->AckInBandIntr();
}

void SdioFunctionDevice::ZirconTransportImpl::IoAbort(IoAbortCompleter::Sync& completer) {
  completer.Reply(parent_->IoAbort());
}

void SdioFunctionDevice::ZirconTransportImpl::IntrPending(IntrPendingCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceIntrPendingResponse response{};
  completer.Reply(parent_->IntrPending(&response));
}

void SdioFunctionDevice::ZirconTransportImpl::DoVendorControlRwByte(
    DoVendorControlRwByteRequestView request, DoVendorControlRwByteCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteResponse response{};
  completer.Reply(parent_->DoVendorControlRwByte(request, &response));
}

void SdioFunctionDevice::ZirconTransportImpl::RegisterVmo(RegisterVmoRequestView request,
                                                          RegisterVmoCompleter::Sync& completer) {
  completer.Reply(parent_->RegisterVmo(request));
}

void SdioFunctionDevice::ZirconTransportImpl::UnregisterVmo(
    UnregisterVmoRequestView request, UnregisterVmoCompleter::Sync& completer) {
  fuchsia_hardware_sdio::wire::DeviceUnregisterVmoResponse response{};
  completer.Reply(parent_->UnregisterVmo(request, &response));
}

void SdioFunctionDevice::ZirconTransportImpl::DoRwTxn(DoRwTxnRequestView request,
                                                      DoRwTxnCompleter::Sync& completer) {
  completer.Reply(parent_->DoRwTxn(request));
}

void SdioFunctionDevice::ZirconTransportImpl::RequestCardReset(
    RequestCardResetCompleter::Sync& completer) {
  completer.Reply(parent_->RequestCardReset());
}

void SdioFunctionDevice::ZirconTransportImpl::PerformTuning(
    PerformTuningCompleter::Sync& completer) {
  completer.Reply(parent_->PerformTuning());
}

zx::result<fuchsia_hardware_sdio::wire::DeviceGetDevHwInfoResponse*>
SdioFunctionDevice::GetDevHwInfo(
    fuchsia_hardware_sdio::wire::DeviceGetDevHwInfoResponse* response) {
  sdio_hw_info_t info{};
  zx_status_t status = sdio_parent_->SdioGetDevHwInfo(function_, &info);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  response->hw_info = {
      .dev_hw_info =
          {
              .num_funcs = info.dev_hw_info.num_funcs,
              .sdio_vsn = info.dev_hw_info.sdio_vsn,
              .cccr_vsn = info.dev_hw_info.cccr_vsn,
              .caps =
                  static_cast<fuchsia_hardware_sdio::SdioDeviceCapabilities>(info.dev_hw_info.caps),
              .max_tran_speed = info.dev_hw_info.max_tran_speed,
          },
      .func_hw_info =
          {
              .manufacturer_id = info.func_hw_info.manufacturer_id,
              .product_id = info.func_hw_info.product_id,
              .max_blk_size = info.func_hw_info.max_blk_size,
              .fn_intf_code = info.func_hw_info.fn_intf_code,
          },
      .host_max_transfer_size = info.host_max_transfer_size,
  };

  return zx::ok(response);
}

zx::result<> SdioFunctionDevice::EnableFn() {
  return zx::make_result(sdio_parent_->SdioEnableFn(function_));
}

zx::result<> SdioFunctionDevice::DisableFn() {
  return zx::make_result(sdio_parent_->SdioDisableFn(function_));
}

zx::result<fuchsia_hardware_sdio::wire::DeviceIoReadyResponse*> SdioFunctionDevice::IoReady(
    fuchsia_hardware_sdio::wire::DeviceIoReadyResponse* response) {
  return zx::make_result(sdio_parent_->SdioIoReady(function_, &response->ready), response);
}

zx::result<> SdioFunctionDevice::EnableFnIntr() {
  return zx::make_result(sdio_parent_->SdioEnableFnIntr(function_));
}

zx::result<> SdioFunctionDevice::DisableFnIntr() {
  return zx::make_result(sdio_parent_->SdioDisableFnIntr(function_));
}

zx::result<> SdioFunctionDevice::UpdateBlockSize(
    fuchsia_hardware_sdio::wire::DeviceUpdateBlockSizeRequest* request) {
  return zx::make_result(
      sdio_parent_->SdioUpdateBlockSize(function_, request->blk_sz, request->deflt));
}

zx::result<fuchsia_hardware_sdio::wire::DeviceGetBlockSizeResponse*>
SdioFunctionDevice::GetBlockSize(
    fuchsia_hardware_sdio::wire::DeviceGetBlockSizeResponse* response) {
  return zx::make_result(sdio_parent_->SdioGetBlockSize(function_, &response->cur_blk_size),
                         response);
}

zx::result<fuchsia_hardware_sdio::wire::DeviceDoRwByteResponse*> SdioFunctionDevice::DoRwByte(
    fuchsia_hardware_sdio::wire::DeviceDoRwByteRequest* request,
    fuchsia_hardware_sdio::wire::DeviceDoRwByteResponse* response) {
  return zx::make_result(sdio_parent_->SdioDoRwByte(request->write, function_, request->addr,
                                                    request->write_byte, &response->read_byte),
                         response);
}

zx::result<fuchsia_hardware_sdio::wire::DeviceGetInBandIntrResponse*>
SdioFunctionDevice::GetInBandIntr(
    fuchsia_hardware_sdio::wire::DeviceGetInBandIntrResponse* response) {
  return zx::make_result(sdio_parent_->SdioGetInBandIntr(function_, &response->irq), response);
}

void SdioFunctionDevice::AckInBandIntr() { sdio_parent_->SdioAckInBandIntr(function_); }

zx::result<> SdioFunctionDevice::IoAbort() {
  return zx::make_result(sdio_parent_->SdioIoAbort(function_));
}

zx::result<fuchsia_hardware_sdio::wire::DeviceIntrPendingResponse*> SdioFunctionDevice::IntrPending(
    fuchsia_hardware_sdio::wire::DeviceIntrPendingResponse* response) {
  return zx::make_result(sdio_parent_->SdioIntrPending(function_, &response->pending), response);
}

zx::result<fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteResponse*>
SdioFunctionDevice::DoVendorControlRwByte(
    fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteRequest* request,
    fuchsia_hardware_sdio::wire::DeviceDoVendorControlRwByteResponse* response) {
  return zx::make_result(
      sdio_parent_->SdioDoVendorControlRwByte(request->write, request->addr, request->write_byte,
                                              &response->read_byte),
      response);
}

zx::result<> SdioFunctionDevice::RegisterVmo(
    fuchsia_hardware_sdio::wire::DeviceRegisterVmoRequest* request) {
  return zx::make_result(sdio_parent_->SdioRegisterVmo(function_, request->vmo_id,
                                                       std::move(request->vmo), request->offset,
                                                       request->size, request->vmo_rights));
}

zx::result<fuchsia_hardware_sdio::wire::DeviceUnregisterVmoResponse*>
SdioFunctionDevice::UnregisterVmo(
    fuchsia_hardware_sdio::wire::DeviceUnregisterVmoRequest* request,
    fuchsia_hardware_sdio::wire::DeviceUnregisterVmoResponse* response) {
  return zx::make_result(
      sdio_parent_->SdioUnregisterVmo(function_, request->vmo_id, &response->vmo), response);
}

zx::result<> SdioFunctionDevice::DoRwTxn(
    fuchsia_hardware_sdio::wire::DeviceDoRwTxnRequest* request) {
  const SdioControllerDevice::SdioRwTxn<fuchsia_hardware_sdmmc::wire::SdmmcBufferRegion> txn{
      .addr = request->txn.addr,
      .incr = request->txn.incr,
      .write = request->txn.write,
      .buffers = {request->txn.buffers.data(), request->txn.buffers.count()},
  };
  return zx::make_result(sdio_parent_->SdioDoRwTxn(function_, txn));
}

zx::result<> SdioFunctionDevice::RequestCardReset() {
  return zx::make_result(sdio_parent_->SdioRequestCardReset());
}

zx::result<> SdioFunctionDevice::PerformTuning() {
  return zx::make_result(sdio_parent_->SdioPerformTuning());
}

fdf::Logger& SdioFunctionDevice::logger() { return sdio_parent_->logger(); }

zx::result<fdf_power::PowerElementConfiguration>
SdioFunctionDevice::GetPowerElementConfiguration() {
  zx::result<fidl::ClientEnd<fuchsia_io::File>> file_client =
      sdio_parent_->parent()->driver_incoming()->Open<fuchsia_io::File>(
          "/pkg/data/sdio_power_config.fidl", fuchsia_io::Flags::kPermReadBytes);
  if (!file_client.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to open SDIO power config: %s", file_client.status_string());
    return file_client.take_error();
  }

  zx::result<fuchsia_hardware_power::ComponentPowerConfiguration> power_config =
      power_config::Load(*std::move(file_client));

  if (power_config->power_elements().size() != 1) {
    FDF_LOGL(ERROR, logger(), "Unexpected number of power elements: %lu",
             power_config->power_elements().size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto converted =
      fdf_power::PowerElementConfiguration::FromFidl(power_config->power_elements()[0]);
  if (converted.is_error()) {
    FDF_LOGL(INFO, logger(), "Failed to convert power element config: %s",
             converted.status_string());
    return converted.take_error();
  }

  if (converted->dependencies.size() != 1) {
    FDF_LOGL(ERROR, logger(), "Unexpected number of power dependencies: %lu",
             converted->dependencies.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const std::string power_element_name = std::format("sdio-function-{}-hardware", function_);
  converted->element.name = power_element_name;
  converted->dependencies[0].child = power_element_name;
  return zx::ok(*std::move(converted));
}

zx::result<> SdioFunctionDevice::ConfigurePowerManagement() {
  zx::result<fdf_power::PowerElementConfiguration> power_element_config =
      GetPowerElementConfiguration();
  if (power_element_config.is_error()) {
    return power_element_config.take_error();
  }

  std::vector<fdf_power::PowerElementConfiguration> configs{
      std::move(power_element_config.value())};

  fit::result<fdf_power::Error, std::vector<fdf_power::ElementDesc>> add_result =
      fdf_power::ApplyPowerConfiguration(*sdio_parent_->parent()->driver_incoming(), configs, true);

  if (add_result.is_error()) {
    std::pair error = MapPowerError(add_result.error_value());
    FDF_LOGL(ERROR, logger(), "Failed to add power element: %s", error.second);
    return error.first;
  }

  if (add_result.value().size() != 1) {
    FDF_LOGL(ERROR, logger(), "Unexpected number of power elements: %lu",
             add_result.value().size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fdf_power::ElementDesc element_desc = std::move(add_result.value()[0]);
  element_runner_server_binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                         std::move(element_desc.element_runner_server.value()),
                                         this, fidl::kIgnoreBindingClosure);

  assertive_token_ = std::move(element_desc.assertive_token);
  element_control_client_end_ = *std::move(element_desc.element_control_client);

  if (fidl::WireResult result =
          fidl::WireCall(*element_desc.lessor_client)->Lease(PowerLevel::kBoot);
      result.ok() && result->is_ok()) {
    boot_level_lease_ = std::move(result->value()->lease_control);
  } else if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Call to Lease failed: %s",
             result.error().FormatDescription().c_str());
    return zx::error(result.error().status());
  } else {
    std::pair error = MapLeaseError(result->error_value());
    FDF_LOGL(ERROR, logger(), "Failed to acquire lease: %s", error.second);
    return error.first;
  }

  return zx::ok();
}

void SdioFunctionDevice::SetLevel(fuchsia_power_broker::wire::ElementRunnerSetLevelRequest* request,
                                  SetLevelCompleter::Sync& completer) {
  switch (request->level) {
    case PowerLevel::kOn:
      // Our client has connected; we should drop the lease we took in order to follow the client's
      // policy from now on. This also causes subsequent calls to SetLevel(OFF) to be respected (see
      // below).
      boot_level_lease_.reset();
      __FALLTHROUGH;
    case PowerLevel::kBoot:
      sdio_parent_->FunctionPowerOn(function_);
      break;
    case PowerLevel::kOff:
      if (!boot_level_lease_.is_valid()) {
        // Power Framework will immediately call SetLevel(OFF) after we've added our power element.
        // We should ignore this call, otherwise it will result in Probe() being called twice during
        // initialization with no corresponding transition in the controller driver.
        sdio_parent_->FunctionPowerOff(function_);
      }
      break;
    default:
      FDF_LOGL(ERROR, logger(), "Unexpected level %u", request->level);
      completer.Close(ZX_ERR_INVALID_ARGS);
      return;
  }

  completer.Reply();
}

void SdioFunctionDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_power_broker::ElementRunner> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOGL(ERROR, logger(), "Unexpected ElementRunner method ordinal 0x%016lx",
           metadata.method_ordinal);
}

void SdioFunctionDevice::GetToken(GetTokenCompleter::Sync& completer) {
  if (!assertive_token_) {
    FDF_LOGL(ERROR, logger(), "Received call to GetToken() but no token is available");
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  fuchsia_hardware_power::PowerTokenProviderGetTokenResponse response;
  zx::event token;
  zx_status_t status = assertive_token_.duplicate(ZX_RIGHT_SAME_RIGHTS, &token);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(token));
  } else {
    completer.Close(status);
  }
}

void SdioFunctionDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_power::PowerTokenProvider> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOGL(ERROR, logger(), "Unexpected PowerTokenProvider method ordinal 0x%016lx",
           metadata.method_ordinal);
}

}  // namespace sdmmc

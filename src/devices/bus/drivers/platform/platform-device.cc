// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/platform-device.h"

#include <assert.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/function.h>
#include <lib/zircon-internal/align.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/syscalls/resource.h>

#include <vector>

#include <fbl/string_printf.h>

#include "src/devices/bus/drivers/platform/node-util.h"
#include "src/devices/bus/drivers/platform/platform-bus.h"
#include "src/devices/bus/drivers/platform/platform-interrupt.h"

namespace {

zx::result<zx_device_prop_t> ConvertToDeviceProperty(
    const fuchsia_driver_framework::NodeProperty& property) {
  if (property.key().Which() != fuchsia_driver_framework::NodePropertyKey::Tag::kIntValue) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  return zx::ok(zx_device_prop_t{static_cast<uint16_t>(property.key().int_value().value()), 0,
                                 property.value().int_value().value()});
}

zx::result<zx_device_str_prop_t> ConvertToDeviceStringProperty(
    const fuchsia_driver_framework::NodeProperty& property) {
  if (property.key().Which() != fuchsia_driver_framework::NodePropertyKey::Tag::kStringValue) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  const char* key = property.key().string_value()->data();
  switch (property.value().Which()) {
    using ValueTag = fuchsia_driver_framework::NodePropertyValue::Tag;
    case ValueTag::kBoolValue: {
      return zx::ok(zx_device_str_prop_t{
          .key = key,
          .property_value = str_prop_bool_val(property.value().bool_value().value()),
      });
    }
    case ValueTag::kIntValue: {
      return zx::ok(zx_device_str_prop_t{
          .key = key,
          .property_value = str_prop_int_val(property.value().int_value().value()),
      });
    }
    case ValueTag::kEnumValue: {
      return zx::ok(zx_device_str_prop_t{
          .key = key,
          .property_value = str_prop_enum_val(property.value().enum_value()->data()),
      });
    }
    case ValueTag::kStringValue: {
      return zx::ok(zx_device_str_prop_t{
          .key = key,
          .property_value = str_prop_str_val(property.value().string_value()->data()),
      });
    }
    default:
      return zx::error(ZX_ERR_INVALID_ARGS);
  }
}

}  // namespace

namespace platform_bus {

namespace fpbus = fuchsia_hardware_platform_bus;

// fuchsia.hardware.platform.bus.PlatformBus implementation.
void RestrictPlatformBus::NodeAdd(NodeAddRequestView request, fdf::Arena& arena,
                                  NodeAddCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void RestrictPlatformBus::ProtocolNodeAdd(ProtocolNodeAddRequestView request, fdf::Arena& arena,
                                          ProtocolNodeAddCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void RestrictPlatformBus::RegisterProtocol(RegisterProtocolRequestView request, fdf::Arena& arena,
                                           RegisterProtocolCompleter::Sync& completer) {
  upstream_->RegisterProtocol(request, arena, completer);
}

void RestrictPlatformBus::GetBoardInfo(fdf::Arena& arena, GetBoardInfoCompleter::Sync& completer) {
  upstream_->GetBoardInfo(arena, completer);
}
void RestrictPlatformBus::SetBoardInfo(SetBoardInfoRequestView request, fdf::Arena& arena,
                                       SetBoardInfoCompleter::Sync& completer) {
  upstream_->SetBoardInfo(request, arena, completer);
}
void RestrictPlatformBus::SetBootloaderInfo(SetBootloaderInfoRequestView request, fdf::Arena& arena,
                                            SetBootloaderInfoCompleter::Sync& completer) {
  upstream_->SetBootloaderInfo(request, arena, completer);
}

void RestrictPlatformBus::RegisterSysSuspendCallback(
    RegisterSysSuspendCallbackRequestView request, fdf::Arena& arena,
    RegisterSysSuspendCallbackCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void RestrictPlatformBus::AddComposite(AddCompositeRequestView request, fdf::Arena& arena,
                                       AddCompositeCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}
void RestrictPlatformBus::AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                                               fdf::Arena& arena,
                                               AddCompositeNodeSpecCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void RestrictPlatformBus::AddCompositeImplicitPbusFragment(
    AddCompositeImplicitPbusFragmentRequestView request, fdf::Arena& arena,
    AddCompositeImplicitPbusFragmentCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

zx_status_t PlatformDevice::Create(fpbus::Node node, zx_device_t* parent, PlatformBus* bus,
                                   Type type, std::unique_ptr<platform_bus::PlatformDevice>* out) {
  fbl::AllocChecker ac;
  std::unique_ptr<platform_bus::PlatformDevice> dev(
      new (&ac) platform_bus::PlatformDevice(parent, bus, type, std::move(node)));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  auto status = dev->Init();
  if (status != ZX_OK) {
    return status;
  }
  out->swap(dev);
  return ZX_OK;
}

PlatformDevice::PlatformDevice(zx_device_t* parent, PlatformBus* bus, Type type, fpbus::Node node)
    : PlatformDeviceType(parent),
      bus_(bus),
      type_(type),
      vid_(node.vid().value_or(0)),
      pid_(node.pid().value_or(0)),
      did_(node.did().value_or(0)),
      instance_id_(node.instance_id().value_or(0)),
      node_(std::move(node)),
      outgoing_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())) {
  strlcpy(name_, node_.name().value_or("no name?").data(), sizeof(name_));
}

zx_status_t PlatformDevice::Init() {
  if (type_ == Protocol) {
    // Protocol devices implement a subset of the platform bus protocol.
    restricted_ = std::make_unique<RestrictPlatformBus>(bus_);
  }

  if (node_.irq().has_value()) {
    for (uint32_t i = 0; i < node_.irq()->size(); i++) {
      auto fragment = std::make_unique<PlatformInterruptFragment>(
          parent(), this, i, fdf::Dispatcher::GetCurrent()->async_dispatcher());
      zx_status_t status = fragment->Add(fbl::StringPrintf("%s-irq%03u", name_, i).data(), this,
                                         node_.irq().value()[i]);
      if (status != ZX_OK) {
        zxlogf(WARNING, "Failed to create interrupt fragment %u", i);
        continue;
      }

      // The DDK takes ownership of the device.
      [[maybe_unused]] auto unused = fragment.release();
    }
  }

  return ZX_OK;
}

zx_status_t PlatformDevice::PDevGetMmio(uint32_t index, pdev_mmio_t* out_mmio) {
  if (node_.mmio() == std::nullopt || index >= node_.mmio()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const auto& mmio = node_.mmio().value()[index];
  if (unlikely(!IsValid(mmio))) {
    return ZX_ERR_INTERNAL;
  }
  if (mmio.base() == std::nullopt) {
    return ZX_ERR_NOT_FOUND;
  }
  const zx_paddr_t vmo_base = ZX_ROUNDDOWN(mmio.base().value(), ZX_PAGE_SIZE);
  const size_t vmo_size =
      ZX_ROUNDUP(mmio.base().value() + mmio.length().value() - vmo_base, ZX_PAGE_SIZE);
  zx::vmo vmo;

  zx_status_t status = zx::vmo::create_physical(*bus_->GetResource(), vmo_base, vmo_size, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: creating vmo failed %d", __FUNCTION__, status);
    return status;
  }

  char name[32];
  snprintf(name, sizeof(name), "mmio %u", index);
  status = vmo.set_property(ZX_PROP_NAME, name, sizeof(name));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: setting vmo name failed %d", __FUNCTION__, status);
    return status;
  }

  out_mmio->offset = mmio.base().value() - vmo_base;
  out_mmio->vmo = vmo.release();
  out_mmio->size = mmio.length().value();
  return ZX_OK;
}

zx_status_t PlatformDevice::PDevGetInterrupt(uint32_t index, uint32_t flags,
                                             zx::interrupt* out_irq) {
  if (node_.irq() == std::nullopt || index >= node_.irq()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (out_irq == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  const auto& irq = node_.irq().value()[index];
  if (unlikely(!IsValid(irq))) {
    return ZX_ERR_INTERNAL;
  }
  if (flags == 0) {
    flags = irq.mode().value();
  }
  zx_status_t status =
      zx::interrupt::create(*bus_->GetResource(), irq.irq().value(), flags, out_irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "platform_dev_map_interrupt: zx_interrupt_create failed %d", status);
    return status;
  }
  return status;
}

zx_status_t PlatformDevice::PDevGetBti(uint32_t index, zx::bti* out_bti) {
  if (node_.bti() == std::nullopt || index >= node_.bti()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (out_bti == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  const auto& bti = node_.bti().value()[index];
  if (unlikely(!IsValid(bti))) {
    return ZX_ERR_INTERNAL;
  }

  return bus_->IommuGetBti(bti.iommu_index().value(), bti.bti_id().value(), out_bti);
}

zx_status_t PlatformDevice::PDevGetSmc(uint32_t index, zx::resource* out_resource) {
  if (node_.smc() == std::nullopt || index >= node_.smc()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (out_resource == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  const auto& smc = node_.smc().value()[index];
  if (unlikely(!IsValid(smc))) {
    return ZX_ERR_INTERNAL;
  }

  uint32_t options = ZX_RSRC_KIND_SMC;
  if (smc.exclusive().value())
    options |= ZX_RSRC_FLAG_EXCLUSIVE;
  char rsrc_name[ZX_MAX_NAME_LEN];
  snprintf(rsrc_name, ZX_MAX_NAME_LEN - 1, "%s.pbus[%u]", name_, index);
  return zx::resource::create(*bus_->GetResource(), options, smc.service_call_num_base().value(),
                              smc.count().value(), rsrc_name, sizeof(rsrc_name), out_resource);
}

zx_status_t PlatformDevice::PDevGetDeviceInfo(pdev_device_info_t* out_info) {
  pdev_device_info_t info = {
      .vid = vid_,
      .pid = pid_,
      .did = did_,
      .mmio_count = static_cast<uint32_t>(node_.mmio().has_value() ? node_.mmio()->size() : 0),
      .irq_count = static_cast<uint32_t>(node_.irq().has_value() ? node_.irq()->size() : 0),
      .bti_count = static_cast<uint32_t>(node_.bti().has_value() ? node_.bti()->size() : 0),
      .smc_count = static_cast<uint32_t>(node_.smc().has_value() ? node_.smc()->size() : 0),
      .metadata_count =
          static_cast<uint32_t>(node_.metadata().has_value() ? node_.metadata()->size() : 0),
      .reserved = {},
      .name = {},
  };
  static_assert(sizeof(info.name) == sizeof(name_), "");
  memcpy(info.name, name_, sizeof(out_info->name));
  memcpy(out_info, &info, sizeof(info));

  return ZX_OK;
}

zx_status_t PlatformDevice::PDevGetBoardInfo(pdev_board_info_t* out_info) {
  auto info = bus_->board_info();
  out_info->pid = info.pid();
  out_info->vid = info.vid();
  out_info->board_revision = info.board_revision();
  strlcpy(out_info->board_name, info.board_name().data(), sizeof(out_info->board_name));
  return ZX_OK;
}

zx_status_t PlatformDevice::PDevDeviceAdd(uint32_t index, const device_add_args_t* args,
                                          zx_device_t** device) {
  return ZX_ERR_NOT_SUPPORTED;
}

// Create a resource and pass it back to the proxy along with necessary metadata
// to create/map the VMO in the driver process.
zx_status_t PlatformDevice::RpcGetMmio(uint32_t index, zx_paddr_t* out_paddr, size_t* out_length,
                                       zx_handle_t* out_handle, uint32_t* out_handle_count) {
  if (node_.mmio() == std::nullopt || index >= node_.mmio()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  const auto& root_rsrc = bus_->GetResource();
  if (!root_rsrc->is_valid()) {
    return ZX_ERR_NO_RESOURCES;
  }

  const auto& mmio = node_.mmio().value()[index];
  if (unlikely(!IsValid(mmio))) {
    return ZX_ERR_INTERNAL;
  }
  zx::resource resource;
  char rsrc_name[ZX_MAX_NAME_LEN];
  snprintf(rsrc_name, ZX_MAX_NAME_LEN - 1, "%s.pbus[%u]", name_, index);
  zx_status_t status =
      zx::resource::create(*root_rsrc, ZX_RSRC_KIND_MMIO, mmio.base().value(),
                           mmio.length().value(), rsrc_name, sizeof(rsrc_name), &resource);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: pdev_rpc_get_mmio: zx_resource_create failed: %d", name_, status);
    return status;
  }

  *out_paddr = mmio.base().value();
  *out_length = mmio.length().value();
  *out_handle_count = 1;
  *out_handle = resource.release();
  return ZX_OK;
}

// Create a resource and pass it back to the proxy along with necessary metadata
// to create the IRQ in the driver process.
zx_status_t PlatformDevice::RpcGetInterrupt(uint32_t index, uint32_t* out_irq, uint32_t* out_mode,
                                            zx_handle_t* out_handle, uint32_t* out_handle_count) {
  if (node_.irq() == std::nullopt || index >= node_.irq()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const auto& root_rsrc = bus_->GetResource();
  if (!root_rsrc->is_valid()) {
    return ZX_ERR_NO_RESOURCES;
  }

  zx::resource resource;
  const auto& irq = node_.irq().value()[index];
  if (unlikely(!IsValid(irq))) {
    return ZX_ERR_INTERNAL;
  }
  uint32_t options = ZX_RSRC_KIND_IRQ | ZX_RSRC_FLAG_EXCLUSIVE;
  char rsrc_name[ZX_MAX_NAME_LEN];
  snprintf(rsrc_name, ZX_MAX_NAME_LEN - 1, "%s.pbus[%u]", name_, index);
  zx_status_t status = zx::resource::create(*root_rsrc, options, irq.irq().value(), 1, rsrc_name,
                                            sizeof(rsrc_name), &resource);
  if (status != ZX_OK) {
    return status;
  }

  *out_irq = irq.irq().value();
  *out_mode = irq.mode().value();
  *out_handle_count = 1;
  *out_handle = resource.release();
  return ZX_OK;
}

zx_status_t PlatformDevice::RpcGetBti(uint32_t index, zx_handle_t* out_handle,
                                      uint32_t* out_handle_count) {
  if (node_.bti() == std::nullopt || index >= node_.bti()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const auto& bti = node_.bti().value()[index];
  if (unlikely(!IsValid(bti))) {
    return ZX_ERR_INTERNAL;
  }

  zx::bti out_bti;
  zx_status_t status = bus_->IommuGetBti(bti.iommu_index().value(), bti.bti_id().value(), &out_bti);
  *out_handle = out_bti.release();

  if (status == ZX_OK) {
    *out_handle_count = 1;
  }

  return status;
}

zx_status_t PlatformDevice::RpcGetSmc(uint32_t index, zx_handle_t* out_handle,
                                      uint32_t* out_handle_count) {
  if (node_.smc() == std::nullopt || index >= node_.smc()->size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  const auto& root_rsrc = bus_->GetResource();
  if (!root_rsrc->is_valid()) {
    return ZX_ERR_NO_RESOURCES;
  }

  zx::resource resource;
  const auto& smc = node_.smc().value()[index];
  if (unlikely(!IsValid(smc))) {
    return ZX_ERR_INTERNAL;
  }
  uint32_t options = ZX_RSRC_KIND_SMC;
  if (smc.exclusive().value())
    options |= ZX_RSRC_FLAG_EXCLUSIVE;
  char rsrc_name[ZX_MAX_NAME_LEN];
  snprintf(rsrc_name, ZX_MAX_NAME_LEN - 1, "%s.pbus[%u]", name_, index);
  zx_status_t status =
      zx::resource::create(*root_rsrc, options, smc.service_call_num_base().value(),
                           smc.count().value(), rsrc_name, sizeof(rsrc_name), &resource);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: pdev_rpc_get_smc: zx_resource_create failed: %d", name_, status);
    return status;
  }

  *out_handle_count = 1;
  *out_handle = resource.release();
  return ZX_OK;
}

zx_status_t PlatformDevice::RpcGetDeviceInfo(pdev_device_info_t* out_info) {
  pdev_device_info_t info = {
      .vid = vid_,
      .pid = pid_,
      .did = did_,
      .mmio_count = static_cast<uint32_t>(node_.mmio().has_value() ? node_.mmio()->size() : 0),
      .irq_count = static_cast<uint32_t>(node_.irq().has_value() ? node_.irq()->size() : 0),
      .bti_count = static_cast<uint32_t>(node_.bti().has_value() ? node_.bti()->size() : 0),
      .smc_count = static_cast<uint32_t>(node_.smc().has_value() ? node_.smc()->size() : 0),
      .metadata_count =
          static_cast<uint32_t>(node_.metadata().has_value() ? node_.metadata()->size() : 0),

      .reserved = {},
      .name = {},
  };
  static_assert(sizeof(info.name) == sizeof(name_), "");
  memcpy(info.name, name_, sizeof(out_info->name));
  memcpy(out_info, &info, sizeof(info));

  return ZX_OK;
}

zx_status_t PlatformDevice::RpcGetMetadata(uint32_t index, uint32_t* out_type, uint8_t* buf,
                                           uint32_t buf_size, uint32_t* actual) {
  size_t normal_metadata = (node_.metadata() == std::nullopt ? 0 : node_.metadata()->size());
  size_t max_metadata =
      normal_metadata + (node_.boot_metadata() == std::nullopt ? 0 : node_.boot_metadata()->size());
  if (index >= max_metadata) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (index < normal_metadata) {
    const auto& metadata = node_.metadata().value()[index];
    if (unlikely(!IsValid(metadata))) {
      return ZX_ERR_INTERNAL;
    }
    if (metadata.data()->size() > buf_size) {
      return ZX_ERR_BUFFER_TOO_SMALL;
    }
    memcpy(buf, metadata.data()->data(), metadata.data()->size());
    *out_type = *metadata.type();
    *actual = static_cast<uint32_t>(metadata.data()->size());
    return ZX_OK;
  }

  // boot_metadata indices follow metadata indices.
  index -= static_cast<uint32_t>(normal_metadata);

  const auto& metadata = node_.boot_metadata().value()[index];
  if (!IsValid(metadata)) {
    return ZX_ERR_INTERNAL;
  }
  zx::result metadata_bi =
      bus_->GetBootItem(metadata.zbi_type().value(), metadata.zbi_extra().value());
  if (metadata_bi.is_error()) {
    return metadata_bi.status_value();
  } else if (metadata_bi->length > buf_size) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }

  auto& [vmo, length] = *metadata_bi;
  auto status = vmo.read(buf, 0, length);
  if (status != ZX_OK) {
    return status;
  }
  *out_type = metadata.zbi_type().value();
  *actual = length;
  return ZX_OK;
}

zx_status_t PlatformDevice::DdkGetProtocol(uint32_t proto_id, void* out) {
  if (proto_id == ZX_PROTOCOL_PDEV) {
    auto proto = static_cast<pdev_protocol_t*>(out);
    proto->ops = &pdev_protocol_ops_;
    proto->ctx = this;
    return ZX_OK;
  } else {
    return bus_->DdkGetProtocol(proto_id, out);
  }
}

zx_status_t PlatformDevice::DdkRxrpc(zx_handle_t channel) {
  if (channel == ZX_HANDLE_INVALID) {
    // proxy device has connected
    return ZX_OK;
  }

  uint8_t req_buf[PROXY_MAX_TRANSFER_SIZE];
  uint8_t resp_buf[PROXY_MAX_TRANSFER_SIZE];
  auto* req_header = reinterpret_cast<platform_proxy_req_t*>(&req_buf);
  auto* resp_header = reinterpret_cast<platform_proxy_rsp_t*>(&resp_buf);
  uint32_t actual;
  zx_handle_t req_handles[ZX_CHANNEL_MAX_MSG_HANDLES];
  zx_handle_t resp_handles[ZX_CHANNEL_MAX_MSG_HANDLES];
  uint32_t req_handle_count;
  uint32_t resp_handle_count = 0;

  auto status = zx_channel_read(channel, 0, &req_buf, req_handles, sizeof(req_buf),
                                std::size(req_handles), &actual, &req_handle_count);
  if (status != ZX_OK) {
    zxlogf(ERROR, "platform_dev_rxrpc: zx_channel_read failed %d", status);
    return status;
  }

  resp_header->txid = req_header->txid;
  uint32_t resp_len;

  auto req = reinterpret_cast<rpc_pdev_req_t*>(&req_buf);
  if (actual < sizeof(*req)) {
    zxlogf(ERROR, "%s received %u, expecting %zu (PDEV)", __func__, actual, sizeof(*req));
    return ZX_ERR_INTERNAL;
  }
  auto resp = reinterpret_cast<rpc_pdev_rsp_t*>(&resp_buf);
  resp_len = sizeof(*resp);

  switch (req_header->op) {
    case PDEV_GET_MMIO:
      status =
          RpcGetMmio(req->index, &resp->paddr, &resp->length, resp_handles, &resp_handle_count);
      break;
    case PDEV_GET_INTERRUPT:
      status =
          RpcGetInterrupt(req->index, &resp->irq, &resp->mode, resp_handles, &resp_handle_count);
      break;
    case PDEV_GET_BTI:
      status = RpcGetBti(req->index, resp_handles, &resp_handle_count);
      break;
    case PDEV_GET_SMC:
      status = RpcGetSmc(req->index, resp_handles, &resp_handle_count);
      break;
    case PDEV_GET_DEVICE_INFO:
      status = RpcGetDeviceInfo(&resp->device_info);
      break;
    case PDEV_GET_BOARD_INFO:
      status = PDevGetBoardInfo(&resp->board_info);
      break;
    case PDEV_GET_METADATA: {
      auto resp = reinterpret_cast<rpc_pdev_metadata_rsp_t*>(resp_buf);
      static_assert(sizeof(*resp) == sizeof(resp_buf), "");
      auto buf_size = static_cast<uint32_t>(sizeof(resp_buf) - sizeof(*resp_header));
      status = RpcGetMetadata(req->index, &resp->pdev.metadata_type, resp->metadata, buf_size,
                              &resp->pdev.metadata_length);
      resp_len += resp->pdev.metadata_length;
      break;
    }
    default:
      zxlogf(ERROR, "%s: unknown pdev op %u", __func__, req_header->op);
      return ZX_ERR_INTERNAL;
  }

  // set op to match request so zx_channel_write will return our response
  resp_header->status = status;
  status = zx_channel_write(channel, 0, resp_header, resp_len,
                            (resp_handle_count ? resp_handles : nullptr), resp_handle_count);
  if (status != ZX_OK) {
    zxlogf(ERROR, "platform_dev_rxrpc: zx_channel_write failed %d", status);
  }
  return status;
}

void PlatformDevice::DdkRelease() { delete this; }

zx_status_t PlatformDevice::Start() {
  char name[ZX_DEVICE_NAME_MAX];
  if (vid_ == PDEV_VID_GENERIC && pid_ == PDEV_PID_GENERIC && did_ == PDEV_DID_KPCI) {
    strlcpy(name, "pci", sizeof(name));
  } else {
    if (instance_id_ == 0) {
      // For backwards compatability, we elide instance id when it is 0.
      snprintf(name, sizeof(name), "%02x:%02x:%01x", vid_, pid_, did_);
    } else {
      snprintf(name, sizeof(name), "%02x:%02x:%01x:%01x", vid_, pid_, did_, instance_id_);
    }
  }

  std::vector<zx_device_prop_t> dev_props{
      {BIND_PLATFORM_DEV_VID, 0, vid_},
      {BIND_PLATFORM_DEV_PID, 0, pid_},
      {BIND_PLATFORM_DEV_DID, 0, did_},
      {BIND_PLATFORM_DEV_INSTANCE_ID, 0, instance_id_},
  };

  std::vector<zx_device_str_prop_t> dev_str_props;
  if (node_.properties().has_value()) {
    for (auto& prop : node_.properties().value()) {
      if (auto dev_prop = ConvertToDeviceProperty(prop); dev_prop.is_ok()) {
        dev_props.emplace_back(dev_prop.value());
      } else if (auto dev_str_prop = ConvertToDeviceStringProperty(prop); dev_str_prop.is_ok()) {
        dev_str_props.emplace_back(dev_str_prop.value());
      } else {
        zxlogf(WARNING, "Node '%s' has unsupported property key type %lu.", name,
               static_cast<unsigned long>(prop.key().Which()));
      }
    }
  }

  ddk::DeviceAddArgs args(name);
  args.set_props(dev_props).set_str_props(dev_str_props).set_proto_id(ZX_PROTOCOL_PDEV);

  std::array fidl_service_offers = {
      fuchsia_hardware_platform_device::Service::Name,
  };
  std::array runtime_service_offers = {
      fuchsia_hardware_platform_bus::Service::Name,
  };

  // Set our FIDL offers.
  {
    zx::result result = outgoing_.AddService<fuchsia_hardware_platform_device::Service>(
        fuchsia_hardware_platform_device::Service::InstanceHandler({
            .device = device_bindings_.CreateHandler(
                this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                fidl::kIgnoreBindingClosure),
        }));
    if (result.is_error()) {
      zxlogf(ERROR, "could not add service to outgoing directory: %s", result.status_string());
      return result.error_value();
    }

    args.set_fidl_service_offers(fidl_service_offers);
  }

  switch (type_) {
    case Protocol: {
      fuchsia_hardware_platform_bus::Service::InstanceHandler handler({
          .platform_bus = bus_bindings_.CreateHandler(
              restricted_.get(), fdf::Dispatcher::GetCurrent()->get(), fidl::kIgnoreBindingClosure),
      });

      zx::result result =
          outgoing_.AddService<fuchsia_hardware_platform_bus::Service>(std::move(handler));
      if (result.is_error()) {
        return result.error_value();
      }

      args.set_runtime_service_offers(runtime_service_offers);
      break;
    }

    case Isolated: {
      // Isolated devices run in separate devhosts.
      // Protocol devices must be in same devhost as platform bus.
      // Composite device fragments are also in the same devhost as platform bus,
      // but the actual composite device will be in a new devhost or devhost belonging to
      // one of the other fragments.
      args.set_flags(DEVICE_ADD_MUST_ISOLATE);
      break;
    }

    case Fragment: {
      break;
    }
  }

  // Setup the outgoing directory.
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  if (zx::result result = outgoing_.Serve(std::move(endpoints->server)); result.is_error()) {
    return result.error_value();
  }
  args.set_outgoing_dir(endpoints->client.TakeChannel());
  return DdkAdd(std::move(args));
}

void PlatformDevice::DdkInit(ddk::InitTxn txn) {
  const size_t metadata_count = node_.metadata() == std::nullopt ? 0 : node_.metadata()->size();
  const size_t boot_metadata_count =
      node_.boot_metadata() == std::nullopt ? 0 : node_.boot_metadata()->size();
  if (metadata_count > 0 || boot_metadata_count > 0) {
    for (size_t i = 0; i < metadata_count; i++) {
      const auto& metadata = node_.metadata().value()[i];
      if (!IsValid(metadata)) {
        txn.Reply(ZX_ERR_INTERNAL);
        return;
      }
      zx_status_t status =
          DdkAddMetadata(metadata.type().value(), metadata.data()->data(), metadata.data()->size());
      if (status != ZX_OK) {
        return txn.Reply(status);
      }
    }

    for (size_t i = 0; i < boot_metadata_count; i++) {
      const auto& metadata = node_.boot_metadata().value()[i];
      if (!IsValid(metadata)) {
        txn.Reply(ZX_ERR_INTERNAL);
        return;
      }
      zx::result data =
          bus_->GetBootItemArray(metadata.zbi_type().value(), metadata.zbi_extra().value());
      zx_status_t status = data.status_value();
      if (data.is_ok()) {
        status = DdkAddMetadata(metadata.zbi_type().value(), data->data(), data->size());
      }
      if (status != ZX_OK) {
        zxlogf(WARNING, "%s failed to add metadata for new device", __func__);
      }
    }
  }
  return txn.Reply(ZX_OK);
}

void PlatformDevice::GetMmio(GetMmioRequestView request, GetMmioCompleter::Sync& completer) {
  pdev_mmio_t banjo_mmio;
  zx_status_t status = PDevGetMmio(request->index, &banjo_mmio);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  fidl::Arena arena;
  fuchsia_hardware_platform_device::wire::Mmio mmio =
      fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
          .offset(banjo_mmio.offset)
          .size(banjo_mmio.size)
          .vmo(zx::vmo(banjo_mmio.vmo))
          .Build();
  completer.ReplySuccess(std::move(mmio));
}

void PlatformDevice::GetInterrupt(GetInterruptRequestView request,
                                  GetInterruptCompleter::Sync& completer) {
  zx::interrupt interrupt;
  zx_status_t status = PDevGetInterrupt(request->index, request->flags, &interrupt);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(interrupt));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) {
  zx::bti bti;
  zx_status_t status = PDevGetBti(request->index, &bti);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(bti));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetSmc(GetSmcRequestView request, GetSmcCompleter::Sync& completer) {
  zx::resource resource;
  zx_status_t status = PDevGetSmc(request->index, &resource);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(resource));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) {
  pdev_device_info_t banjo_info;
  zx_status_t status = PDevGetDeviceInfo(&banjo_info);
  if (status == ZX_OK) {
    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::DeviceInfo::Builder(arena)
                               .vid(banjo_info.vid)
                               .pid(banjo_info.pid)
                               .did(banjo_info.did)
                               .mmio_count(banjo_info.mmio_count)
                               .irq_count(banjo_info.irq_count)
                               .bti_count(banjo_info.bti_count)
                               .smc_count(banjo_info.smc_count)
                               .metadata_count(banjo_info.metadata_count)
                               .name(banjo_info.name)
                               .Build());
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetBoardInfo(GetBoardInfoCompleter::Sync& completer) {
  pdev_board_info_t banjo_info;
  zx_status_t status = PDevGetBoardInfo(&banjo_info);
  if (status == ZX_OK) {
    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                               .vid(banjo_info.vid)
                               .pid(banjo_info.pid)
                               .board_name(banjo_info.board_name)
                               .board_revision(banjo_info.board_revision)
                               .Build());
  } else {
    completer.ReplyError(status);
  }
}

}  // namespace platform_bus

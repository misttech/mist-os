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

zx_status_t PlatformDevice::DdkGetProtocol(uint32_t proto_id, void* out) {
  return bus_->DdkGetProtocol(proto_id, out);
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

zx::result<PlatformDevice::Mmio> PlatformDevice::GetMmio(uint32_t index) {
  if (node_.mmio() == std::nullopt || index >= node_.mmio()->size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const auto& mmio = node_.mmio().value()[index];
  if (unlikely(!IsValid(mmio))) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (mmio.base() == std::nullopt) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  const zx_paddr_t vmo_base = ZX_ROUNDDOWN(mmio.base().value(), ZX_PAGE_SIZE);
  const size_t vmo_size =
      ZX_ROUNDUP(mmio.base().value() + mmio.length().value() - vmo_base, ZX_PAGE_SIZE);
  zx::vmo vmo;

  zx_status_t status = zx::vmo::create_physical(*bus_->GetResource(), vmo_base, vmo_size, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: creating vmo failed %d", __FUNCTION__, status);
    return zx::error(status);
  }

  char name[32];
  snprintf(name, sizeof(name), "mmio %u", index);
  status = vmo.set_property(ZX_PROP_NAME, name, sizeof(name));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: setting vmo name failed %d", __FUNCTION__, status);
    return zx::error(status);
  }
  return zx::ok(PlatformDevice::Mmio{
      .offset = mmio.base().value() - vmo_base,
      .size = mmio.length().value(),
      .vmo = std::move(vmo),
  });
}

void PlatformDevice::GetMmio(GetMmioRequestView request, GetMmioCompleter::Sync& completer) {
  zx::result result = GetMmio(request->index);
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
    return;
  }

  fidl::Arena arena;
  fuchsia_hardware_platform_device::wire::Mmio mmio =
      fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
          .offset(result->offset)
          .size(result->size)
          .vmo(std::move(result->vmo))
          .Build();
  completer.ReplySuccess(std::move(mmio));
}

zx::result<zx::interrupt> PlatformDevice::GetInterrupt(uint32_t index, uint32_t flags) {
  if (node_.irq() == std::nullopt || index >= node_.irq()->size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const auto& irq = node_.irq().value()[index];
  if (unlikely(!IsValid(irq))) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  if (flags == 0) {
    flags = irq.mode().value();
  }

  zx::interrupt out_irq;
  zx_status_t status =
      zx::interrupt::create(*bus_->GetResource(), irq.irq().value(), flags, &out_irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "platform_dev_map_interrupt: zx_interrupt_create failed %d", status);
    return zx::error(status);
  }
  return zx::ok(std::move(out_irq));
}

void PlatformDevice::GetInterrupt(GetInterruptRequestView request,
                                  GetInterruptCompleter::Sync& completer) {
  zx::result result = GetInterrupt(request->index, request->flags);
  if (result.is_ok()) {
    completer.ReplySuccess(std::move(*result));
  } else {
    completer.ReplyError(result.status_value());
  }
}

zx::result<zx::bti> PlatformDevice::GetBti(uint32_t index) {
  if (node_.bti() == std::nullopt || index >= node_.bti()->size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }
  const auto& bti = node_.bti().value()[index];
  if (unlikely(!IsValid(bti))) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::bti out_bti;
  zx_status_t status = bus_->IommuGetBti(bti.iommu_index().value(), bti.bti_id().value(), &out_bti);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(out_bti));
}

void PlatformDevice::GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) {
  zx::result result = GetBti(request->index);
  if (result.is_ok()) {
    completer.ReplySuccess(std::move(*result));
  } else {
    completer.ReplyError(result.status_value());
  }
}

zx::result<zx::resource> PlatformDevice::GetSmc(uint32_t index) {
  if (node_.smc() == std::nullopt || index >= node_.smc()->size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const auto& smc = node_.smc().value()[index];
  if (unlikely(!IsValid(smc))) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  uint32_t options = ZX_RSRC_KIND_SMC;
  if (smc.exclusive().value())
    options |= ZX_RSRC_FLAG_EXCLUSIVE;
  char rsrc_name[ZX_MAX_NAME_LEN];
  snprintf(rsrc_name, ZX_MAX_NAME_LEN - 1, "%s.pbus[%u]", name_, index);

  zx::resource out_resource;
  zx_status_t status =
      zx::resource::create(*bus_->GetResource(), options, smc.service_call_num_base().value(),
                           smc.count().value(), rsrc_name, sizeof(rsrc_name), &out_resource);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(out_resource));
}

void PlatformDevice::GetSmc(GetSmcRequestView request, GetSmcCompleter::Sync& completer) {
  zx::result result = GetSmc(request->index);
  if (result.is_ok()) {
    completer.ReplySuccess(std::move(*result));
  } else {
    completer.ReplyError(result.status_value());
  }
}

void PlatformDevice::GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) {
  fidl::Arena arena;
  completer.ReplySuccess(
      fuchsia_hardware_platform_device::wire::DeviceInfo::Builder(arena)
          .vid(vid_)
          .pid(pid_)
          .did(did_)
          .mmio_count(static_cast<uint32_t>(node_.mmio().has_value() ? node_.mmio()->size() : 0))
          .irq_count(static_cast<uint32_t>(node_.irq().has_value() ? node_.irq()->size() : 0))
          .bti_count(static_cast<uint32_t>(node_.bti().has_value() ? node_.bti()->size() : 0))
          .smc_count(static_cast<uint32_t>(node_.smc().has_value() ? node_.smc()->size() : 0))
          .metadata_count(
              static_cast<uint32_t>(node_.metadata().has_value() ? node_.metadata()->size() : 0))
          .name(name_)
          .Build());
}

void PlatformDevice::GetBoardInfo(GetBoardInfoCompleter::Sync& completer) {
  auto info = bus_->board_info();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                             .vid(info.pid())
                             .pid(info.pid())
                             .board_name(info.board_name())
                             .board_revision(info.board_revision())
                             .Build());
}

}  // namespace platform_bus

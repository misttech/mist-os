// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/platform-device.h"

#include <assert.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/ddk/binding_driver.h>
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
#include <zircon/errors.h>
#include <zircon/syscalls/resource.h>

#include <unordered_set>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/resource/cpp/bind.h>
#include <fbl/string_printf.h>

#include "lib/inspect/component/cpp/component.h"
#include "src/devices/bus/drivers/platform/node-util.h"
#include "src/devices/bus/drivers/platform/platform-bus.h"
#include "src/devices/bus/drivers/platform/platform-interrupt.h"

namespace {

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
void RestrictPlatformBus::AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                                               fdf::Arena& arena,
                                               AddCompositeNodeSpecCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void RestrictPlatformBus::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_platform_bus::PlatformBus> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(WARNING, "RestrictPlatformBus received unknown method with ordinal: %lu",
         metadata.method_ordinal);
}

zx_status_t PlatformDevice::Create(fpbus::Node node, zx_device_t* parent, PlatformBus* bus,
                                   Type type, inspect::ComponentInspector& inspector,
                                   std::unique_ptr<platform_bus::PlatformDevice>* out) {
  auto inspect_node_name = std::string{node.name().value_or("unnamed")} + "-platform-device";
  fbl::AllocChecker ac;
  std::unique_ptr<platform_bus::PlatformDevice> dev(new (&ac) platform_bus::PlatformDevice(
      parent, bus, type, inspector.root().CreateChild(inspect_node_name), std::move(node)));
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

fpromise::promise<inspect::Inspector> PlatformDevice::InspectNodeCallback() const {
  inspect::Inspector inspector;
  auto interrupt_vectors =
      inspector.GetRoot().CreateUintArray("interrupt_vectors", interrupt_vectors_.size());
  for (size_t i = 0; i < interrupt_vectors_.size(); ++i) {
    interrupt_vectors.Set(i, interrupt_vectors_[i]);
  }
  inspector.emplace(std::move(interrupt_vectors));
  return fpromise::make_result_promise(fpromise::ok(std::move(inspector)));
}

PlatformDevice::PlatformDevice(zx_device_t* parent, PlatformBus* bus, Type type,
                               inspect::Node inspect_node, fpbus::Node node)
    : PlatformDeviceType(parent),
      bus_(bus),
      type_(type),
      vid_(node.vid().value_or(0)),
      pid_(node.pid().value_or(0)),
      did_(node.did().value_or(0)),
      instance_id_(node.instance_id().value_or(0)),
      node_(std::move(node)),
      outgoing_(fdf::OutgoingDirectory::Create(fdf::Dispatcher::GetCurrent()->get())),
      inspect_node_(std::move(inspect_node)) {
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

  inspect_node_.RecordLazyValues("interrupt_vectors",
                                 fit::bind_member<&PlatformDevice::InspectNodeCallback>(this));

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

  zx_status_t status = zx::vmo::create_physical(*bus_->GetMmioResource(), vmo_base, vmo_size, &vmo);
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
    flags = static_cast<uint32_t>(irq.mode().value());
  }
  auto vector = irq.irq().value();
  zxlogf(INFO, "Creating interrupt with vector %u for platform device \"%s\"", vector, name_);
  zx_status_t status = zx::interrupt::create(*bus_->GetIrqResource(), vector, flags, out_irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "platform_dev_map_interrupt: zx_interrupt_create failed %d", status);
    return status;
  }
  interrupt_vectors_.emplace_back(vector);
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
  return zx::resource::create(*bus_->GetSmcResource(), options, smc.service_call_num_base().value(),
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

zx_status_t PlatformDevice::DdkGetProtocol(uint32_t proto_id, void* out) {
  if (proto_id == ZX_PROTOCOL_PDEV) {
    auto proto = static_cast<pdev_protocol_t*>(out);
    proto->ops = &pdev_protocol_ops_;
    proto->ctx = this;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

void PlatformDevice::DdkRelease() { delete this; }

zx_status_t PlatformDevice::Start() {
  // TODO(b/340283894): Remove.
  static const std::unordered_set<std::string> kLegacyNameAllowlist{
      "ram-nand",         // 00:00:2e
      "aml-thermal-pll",  // 05:05:a,05:03:a,05:04:a
      "thermistor",       // 03:0a:27
      "pll-temp-sensor",  // 05:06:39
      "sysmem",           // 00:00:1b
  };

  char name[ZX_DEVICE_NAME_MAX];
  if (vid_ == PDEV_VID_GENERIC && pid_ == PDEV_PID_GENERIC && did_ == PDEV_DID_KPCI) {
    strlcpy(name, "pci", sizeof(name));
  } else if (did_ == PDEV_DID_DEVICETREE_NODE) {
    strlcpy(name, name_, sizeof(name));
  } else {
    // TODO(b/340283894): Remove legacy name format once `kLegacyNameAllowlist` is removed.
    if (kLegacyNameAllowlist.find(name_) != kLegacyNameAllowlist.end()) {
      if (instance_id_ == 0) {
        // For backwards compatibility, we elide instance id when it is 0.
        snprintf(name, sizeof(name), "%02x:%02x:%01x", vid_, pid_, did_);
      } else {
        snprintf(name, sizeof(name), "%02x:%02x:%01x:%01x", vid_, pid_, did_, instance_id_);
      }
    } else {
      strlcpy(name, name_, sizeof(name));
    }
  }

  std::vector<zx_device_str_prop_t> dev_str_props{
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_VID, vid_),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_PID, pid_),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_DID, did_),
      ddk::MakeStrProperty(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id_),
  };
  if (node_.properties().has_value()) {
    for (auto& prop : node_.properties().value()) {
      if (prop.key().Which() == fuchsia_driver_framework::NodePropertyKey::Tag::kIntValue) {
        zxlogf(WARNING, "Node '%s' has unsupported property integer-key %u.", name,
               prop.key().int_value().value());
      } else if (auto dev_str_prop = ConvertToDeviceStringProperty(prop); dev_str_prop.is_ok()) {
        dev_str_props.emplace_back(dev_str_prop.value());
      } else {
        zxlogf(WARNING, "Node '%s' has unsupported property key type %lu.", name,
               static_cast<unsigned long>(prop.key().Which()));
      }
    }
  }

  // Need to keep keys and vals alive until DdkAdd is called.
  std::vector<std::string> keys_and_vals;
  auto count_props = [](const auto& r) { return r.has_value() ? r->size() : 0u; };
  keys_and_vals.reserve(2lu * (count_props(node_.mmio()) + count_props(node_.irq()) +
                               count_props(node_.bti()) + count_props(node_.smc())));

  auto add_props = [&dev_str_props, &keys_and_vals](const auto& resource,
                                                    const std::string& count_key,
                                                    const char* resource_key_prefix) {
    const uint32_t count = resource.has_value() ? static_cast<uint32_t>(resource->size()) : 0u;
    dev_str_props.emplace_back(ddk::MakeStrProperty(count_key, count));

    for (uint32_t i = 0; i < count; i++) {
      const auto& name = resource.value()[i].name();
      const std::string& key = keys_and_vals.emplace_back(resource_key_prefix + std::to_string(i));
      const std::string& value =
          keys_and_vals.emplace_back(name.has_value() ? name.value() : "unknown");
      dev_str_props.emplace_back(ddk::MakeStrProperty(key, value));
    }
  };
  add_props(node_.mmio(), bind_fuchsia_resource::MMIO_COUNT, "fuchsia.resource.MMIO_");
  add_props(node_.irq(), bind_fuchsia_resource::INTERRUPT_COUNT, "fuchsia.resource.INTERRUPT_");
  add_props(node_.bti(), bind_fuchsia_resource::BTI_COUNT, "fuchsia.resource.BTI_");
  add_props(node_.smc(), bind_fuchsia_resource::SMC_COUNT, "fuchsia.resource.SMC_");

  ddk::DeviceAddArgs args(name);
  args.set_str_props(dev_str_props).set_proto_id(ZX_PROTOCOL_PDEV);

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
  return DdkAdd(args);
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

      auto metadata_id = metadata.id();
      ZX_ASSERT(metadata_id.has_value());
      auto metadata_data = metadata.data();
      ZX_ASSERT(metadata_data.has_value());

      // TODO(b/341981272): Remove `DdkAddMetadata()` once all drivers bound to platform devices do
      // not use `device_get_metadata()` to retrieve metadata. They should be using
      // fuchsia.hardware.platform.device/Device::GetMetadata().
      errno = 0;
      char* metadata_id_end{};
      const char* metadata_id_start = metadata_id.value().c_str();
      auto metadata_type =
          static_cast<uint32_t>(std::strtol(metadata_id_start, &metadata_id_end, 10));
      if (!metadata_id.value().empty() && errno == 0 && *metadata_id_end == '\0') {
        zx_status_t status =
            DdkAddMetadata(metadata_type, metadata_data->data(), metadata_data->size());
        if (status != ZX_OK) {
          txn.Reply(status);
          return;
        }
      }

      metadata_.emplace(metadata_id.value(), metadata_data.value());
    }

    for (size_t i = 0; i < boot_metadata_count; i++) {
      const auto& metadata = node_.boot_metadata().value()[i];
      if (!IsValid(metadata)) {
        txn.Reply(ZX_ERR_INTERNAL);
        return;
      }

      auto metadata_zbi_type = metadata.zbi_type();
      ZX_ASSERT(metadata_zbi_type.has_value());

      zx::result data =
          bus_->GetBootItemArray(metadata_zbi_type.value(), metadata.zbi_extra().value());
      zx_status_t status = data.status_value();
      if (data.is_ok()) {
        // TODO(b/341981272): Remove `DdkAddMetadata()` once all drivers bound to platform devices
        // do not use `device_get_metadata()` to retrieve metadata. They should be using
        // fuchsia.hardware.platform.device/Device::GetMetadata().
        status = DdkAddMetadata(metadata_zbi_type.value(), data->data(), data->size());
        if (status != ZX_OK) {
          zxlogf(WARNING, "%s failed to add metadata for new device", __func__);
        }

        metadata_.emplace(std::to_string(metadata_zbi_type.value()),
                          std::vector<uint8_t>{data->begin(), data->end()});
      }
    }
  }
  return txn.Reply(ZX_OK);
}

void PlatformDevice::GetMmioById(GetMmioByIdRequestView request,
                                 GetMmioByIdCompleter::Sync& completer) {
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

void PlatformDevice::GetMmioByName(GetMmioByNameRequestView request,
                                   GetMmioByNameCompleter::Sync& completer) {
  if (request->name.empty()) {
    return completer.ReplyError(ZX_ERR_INVALID_ARGS);
  }
  std::optional<uint32_t> index = GetMmioIndex(node_, request->name.get());
  if (!index.has_value()) {
    return completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  pdev_mmio_t banjo_mmio;
  zx_status_t status = PDevGetMmio(index.value(), &banjo_mmio);
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

void PlatformDevice::GetInterruptById(GetInterruptByIdRequestView request,
                                      GetInterruptByIdCompleter::Sync& completer) {
  zx::interrupt interrupt;
  zx_status_t status = PDevGetInterrupt(request->index, request->flags, &interrupt);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(interrupt));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetInterruptByName(GetInterruptByNameRequestView request,
                                        GetInterruptByNameCompleter::Sync& completer) {
  if (request->name.empty()) {
    return completer.ReplyError(ZX_ERR_INVALID_ARGS);
  }
  std::optional<uint32_t> index = GetIrqIndex(node_, request->name.get());
  if (!index.has_value()) {
    return completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
  }
  zx::interrupt interrupt;
  zx_status_t status = PDevGetInterrupt(index.value(), request->flags, &interrupt);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(interrupt));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetBtiById(GetBtiByIdRequestView request,
                                GetBtiByIdCompleter::Sync& completer) {
  zx::bti bti;
  zx_status_t status = PDevGetBti(request->index, &bti);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(bti));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetBtiByName(GetBtiByNameRequestView request,
                                  GetBtiByNameCompleter::Sync& completer) {
  if (request->name.empty()) {
    return completer.ReplyError(ZX_ERR_INVALID_ARGS);
  }
  std::optional<uint32_t> index = GetBtiIndex(node_, request->name.get());
  if (!index.has_value()) {
    return completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
  }
  zx::bti bti;
  zx_status_t status = PDevGetBti(index.value(), &bti);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(bti));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetSmcById(GetSmcByIdRequestView request,
                                GetSmcByIdCompleter::Sync& completer) {
  zx::resource resource;
  zx_status_t status = PDevGetSmc(request->index, &resource);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(resource));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetSmcByName(GetSmcByNameRequestView request,
                                  GetSmcByNameCompleter::Sync& completer) {
  if (request->name.empty()) {
    return completer.ReplyError(ZX_ERR_INVALID_ARGS);
  }
  std::optional<uint32_t> index = GetSmcIndex(node_, request->name.get());
  if (!index.has_value()) {
    return completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
  }
  zx::resource resource;
  zx_status_t status = PDevGetSmc(index.value(), &resource);
  if (status == ZX_OK) {
    completer.ReplySuccess(std::move(resource));
  } else {
    completer.ReplyError(status);
  }
}

void PlatformDevice::GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) {
  std::optional<std::vector<fuchsia_hardware_power::PowerElementConfiguration>> config =
      node_.power_config();
  if (config.has_value()) {
    auto element_configs = config.value();
    fidl::Arena arena;
    fidl::VectorView<fuchsia_hardware_power::wire::PowerElementConfiguration> elements;
    elements.Allocate(arena, element_configs.size());

    size_t offset = 0;
    for (auto& config : element_configs) {
      fuchsia_hardware_power::wire::PowerElementConfiguration wire_config =
          fidl::ToWire(arena, config);
      elements.at(offset) = wire_config;

      offset++;
    }
    completer.ReplySuccess(elements);

  } else {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
  }
}

void PlatformDevice::GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) {
  pdev_device_info_t banjo_info;
  zx_status_t status = PDevGetDeviceInfo(&banjo_info);
  if (status == ZX_OK) {
    fidl::Arena arena;
    completer.ReplySuccess(fuchsia_hardware_platform_device::wire::NodeDeviceInfo::Builder(arena)
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

void PlatformDevice::GetMetadata(GetMetadataRequestView request,
                                 GetMetadataCompleter::Sync& completer) {
  auto id = std::to_string(request->type);
  auto metadata = metadata_.find(id);
  if (metadata == metadata_.end()) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(metadata->second));
}

void PlatformDevice::GetMetadata2(GetMetadata2RequestView request,
                                  GetMetadata2Completer::Sync& completer) {
  if (auto metadata = metadata_.find(request->id.get()); metadata != metadata_.end()) {
    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(metadata->second));
    return;
  }

  completer.ReplyError(ZX_ERR_NOT_FOUND);
}

void PlatformDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(WARNING, "PlatformDevice received unknown method with ordinal: %lu",
         metadata.method_ordinal);
}

}  // namespace platform_bus

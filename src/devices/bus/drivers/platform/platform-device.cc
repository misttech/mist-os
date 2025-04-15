// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bus/drivers/platform/platform-device.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fit/function.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/zbi-format/partition.h>
#include <lib/zircon-internal/align.h>
#include <zircon/errors.h>
#include <zircon/syscalls/resource.h>
#include <zircon/system/public/zircon/syscalls-next.h>

#include <unordered_set>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/resource/cpp/bind.h>
#include <fbl/algorithm.h>

#include "src/devices/bus/drivers/platform/node-util.h"
#include "src/devices/bus/drivers/platform/platform-bus.h"
#include "src/devices/bus/drivers/platform/platform-interrupt.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace {

fuchsia_boot_metadata::SerialNumberMetadata CreateSerialNumberMetadata(
    const fbl::Array<uint8_t>& bytes) {
  std::string serial_number{bytes.begin(), bytes.end()};
  return fuchsia_boot_metadata::SerialNumberMetadata{{.serial_number{std::move(serial_number)}}};
}

zx::result<fuchsia_boot_metadata::PartitionMapMetadata> CreatePartitionMapMetadata(
    const fbl::Array<uint8_t>& bytes) {
  if (bytes.size() < sizeof(zbi_partition_map_t)) {
    fdf::error("Incorrect number of bytes: Expected at least {} bytes but actual is {} bytes",
               sizeof(zbi_partition_map_t), bytes.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  const auto* partition_map_entries = reinterpret_cast<zbi_partition_map_t*>(bytes.data());
  auto partition_count = partition_map_entries[0].partition_count;
  auto minimum_num_bytes = partition_count * sizeof(zbi_partition_map_t);
  if (bytes.size() < minimum_num_bytes) {
    fdf::error("Incorrect number of bytes: Expected at least {} bytes but actual is {} bytes",
               minimum_num_bytes, bytes.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fuchsia_boot_metadata::PartitionMapEntry> partition_map;
  for (uint32_t i = 0; i < partition_count; ++i) {
    const auto& entry = partition_map_entries[i];
    std::array<uint8_t, fuchsia_boot_metadata::kPartitionGuidLen> guid;
    static_assert(fuchsia_boot_metadata::kPartitionGuidLen >= sizeof(zbi_partition_guid_t));
    std::ranges::copy(std::begin(entry.guid), std::end(entry.guid), guid.begin());
    partition_map.emplace_back(
        fuchsia_boot_metadata::PartitionMapEntry{{.block_count = entry.block_count,
                                                  .block_size = entry.block_size,
                                                  .partition_count = entry.partition_count,
                                                  .reserved = entry.reserved,
                                                  .guid = guid}});
  }

  return zx::ok(
      fuchsia_boot_metadata::PartitionMapMetadata{{.partition_map = std::move(partition_map)}});
}

zx::result<fuchsia_boot_metadata::MacAddressMetadata> CreateMacAddressMetadata(
    const fbl::Array<uint8_t>& bytes) {
  fuchsia_net::MacAddress mac_address;
  if (bytes.size() != mac_address.octets().size()) {
    fdf::error("Size of encoded MAC address is incorrect: expected {} bytes but actual is {} bytes",
               mac_address.octets().size(), bytes.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  std::ranges::copy(bytes.begin(), bytes.end(), mac_address.octets().begin());
  return zx::ok(fuchsia_boot_metadata::MacAddressMetadata{{.mac_address = std::move(mac_address)}});
}

}  // namespace

namespace platform_bus {

namespace fpbus = fuchsia_hardware_platform_bus;

zx::result<std::unique_ptr<PlatformDevice>> PlatformDevice::Create(
    fpbus::Node node, PlatformBus* bus, inspect::ComponentInspector& inspector) {
  auto inspect_node_name = node.name().value() + "-platform-device";
  auto dev = std::make_unique<platform_bus::PlatformDevice>(
      bus, inspector.root().CreateChild(inspect_node_name), std::move(node));
  zx::result result = dev->Init();
  if (result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::move(dev));
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

PlatformDevice::PlatformDevice(PlatformBus* bus, inspect::Node inspect_node, fpbus::Node node)
    : bus_(bus),
      name_(node.name().value()),
      vid_(node.vid().value_or(0)),
      pid_(node.pid().value_or(0)),
      did_(node.did().value_or(0)),
      instance_id_(node.instance_id().value_or(0)),
      node_(std::move(node)),
      inspect_node_(std::move(inspect_node)),
      serial_number_metadata_server_(name_),
      partition_map_metadata_server_(name_),
      mac_address_metadata_server_(name_) {}

zx::result<PlatformDevice::Mmio> PlatformDevice::GetMmio(uint32_t index) const {
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
  const zx_paddr_t vmo_base = fbl::round_down(mmio.base().value(), zx_system_get_page_size());
  const size_t vmo_size = fbl::round_up(mmio.base().value() + mmio.length().value() - vmo_base,
                                        zx_system_get_page_size());
  zx::vmo vmo;

  zx_status_t status = zx::vmo::create_physical(*bus_->GetMmioResource(), vmo_base, vmo_size, &vmo);
  if (status != ZX_OK) {
    fdf::error("creating vmo failed {}", zx_status_get_string(status));
    return zx::error(status);
  }

  char name[32]{};
  std::format_to_n(name, sizeof(name) - 1, "mmio {}", index);
  status = vmo.set_property(ZX_PROP_NAME, name, sizeof(name));
  if (status != ZX_OK) {
    fdf::error("setting vmo name failed {}", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(Mmio{
      .offset = mmio.base().value() - vmo_base,
      .size = mmio.length().value(),
      .vmo = vmo.release(),
  });
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
    flags = static_cast<uint32_t>(irq.mode().value());
  }
  if (flags & ZX_INTERRUPT_WAKE_VECTOR) {
    fdf::warn("Client passing in ZX_INTERRUPT_WAKE_VECTOR. This will be an error in the future.");
  }
  if (bus_->suspend_enabled() && irq.wake_vector().has_value() && irq.wake_vector().value()) {
    flags &= ZX_INTERRUPT_WAKE_VECTOR;
  }
  auto vector = irq.irq().value();
  fdf::info("Creating interrupt with vector {} for platform device \"{}\"", vector, name_);
  zx::interrupt out_irq;
  zx_status_t status = zx::interrupt::create(*bus_->GetIrqResource(), vector, flags, &out_irq);
  if (status != ZX_OK) {
    fdf::error("zx_interrupt_create failed {}", zx_status_get_string(status));
    return zx::error(status);
  }
  interrupt_vectors_.emplace_back(vector);
  return zx::ok(std::move(out_irq));
}

zx::result<zx::bti> PlatformDevice::GetBti(uint32_t index) const {
  if (node_.bti() == std::nullopt || index >= node_.bti()->size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const auto& bti = node_.bti().value()[index];
  if (unlikely(!IsValid(bti))) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  return bus_->GetBti(bti.iommu_index().value(), bti.bti_id().value());
}

zx::result<zx::resource> PlatformDevice::GetSmc(uint32_t index) const {
  if (node_.smc() == std::nullopt || index >= node_.smc()->size()) {
    return zx::error(ZX_ERR_OUT_OF_RANGE);
  }

  const auto& smc = node_.smc().value()[index];
  if (unlikely(!IsValid(smc))) {
    return zx::error(ZX_ERR_INTERNAL);
  }

  uint32_t options = ZX_RSRC_KIND_SMC;
  if (smc.exclusive().value()) {
    options |= ZX_RSRC_FLAG_EXCLUSIVE;
  }
  char rsrc_name[ZX_MAX_NAME_LEN] = {};
  std::format_to_n(rsrc_name, sizeof(rsrc_name) - 1, "{}.pbus[{}]", name_, index);
  zx::resource out_resource;
  zx_status_t status =
      zx::resource::create(*bus_->GetSmcResource(), options, smc.service_call_num_base().value(),
                           smc.count().value(), rsrc_name, sizeof(rsrc_name), &out_resource);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(out_resource));
}

PlatformDevice::DeviceInfo PlatformDevice::GetDeviceInfo() const {
  return DeviceInfo{
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
      .name = name_,
  };
}

PlatformDevice::BoardInfo PlatformDevice::GetBoardInfo() const {
  auto info = bus_->board_info();
  return BoardInfo{
      .vid = info.vid(),
      .pid = info.pid(),
      .board_revision = info.board_revision(),
      .board_name = info.board_name(),
  };
}

zx::result<> PlatformDevice::CreateNode() {
  // TODO(b/340283894): Remove.
  static const std::unordered_set<std::string> kLegacyNameAllowlist{
      "aml-thermal-pll",  // 05:05:a,05:03:a,05:04:a
      "thermistor",       // 03:0a:27
      "pll-temp-sensor",  // 05:06:39
  };

  std::optional<fdf::DeviceAddress> address;
  auto bus_type = fdf::BusType::kPlatform;

  std::string name;
  if (vid_ == PDEV_VID_GENERIC && pid_ == PDEV_PID_GENERIC && did_ == PDEV_DID_KPCI) {
    name = "pci";
    address = fdf::DeviceAddress::WithStringValue("pci");
  } else if (did_ == PDEV_DID_DEVICETREE_NODE) {
    name = name_;
    bus_type = fdf::BusType::kDeviceTree;
    address = fdf::DeviceAddress::WithStringValue(name_);
  } else {
    // TODO(b/340283894): Remove legacy name format once `kLegacyNameAllowlist` is removed.
    if (kLegacyNameAllowlist.find(name_) != kLegacyNameAllowlist.end()) {
      if (instance_id_ == 0) {
        // For backwards compatibility, we elide instance id when it is 0.
        name = std::format("{:02x}_{:02x}_{:01x}", vid_, pid_, did_);
        address = fdf::DeviceAddress::WithArrayIntValue(
            {static_cast<uint8_t>(vid_), static_cast<uint8_t>(pid_), static_cast<uint8_t>(did_)});
      } else {
        name = std::format("{:02x}_{:02x}_{:01x}_{:01x}", vid_, pid_, did_, instance_id_);
        address = fdf::DeviceAddress::WithArrayIntValue(
            {static_cast<uint8_t>(vid_), static_cast<uint8_t>(pid_), static_cast<uint8_t>(did_),
             static_cast<uint8_t>(instance_id_)});
      }
    } else {
      name = name_;
      address = fdf::DeviceAddress::WithStringValue(name_);
    }
  }

  auto bus_info = fdf::BusInfo{{
      .bus = bus_type,
      .address = address,
      .address_stability = fdf::DeviceAddressStability::kStable,
  }};

  std::vector props{
      fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_VID, vid_),
      fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_PID, pid_),
      fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_DID, did_),
      fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, instance_id_),
      fdf::MakeProperty2(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE)};
  if (const auto& node_props = node_.properties(); node_props.has_value()) {
    std::copy(node_props->cbegin(), node_props->cend(), std::back_inserter(props));
  }

  auto add_props = [&props](const auto& resource, const std::string& count_key,
                            const char* resource_key_prefix) {
    const uint32_t count = resource.has_value() ? static_cast<uint32_t>(resource->size()) : 0u;
    props.emplace_back(fdf::MakeProperty2(count_key, count));

    for (uint32_t i = 0; i < count; i++) {
      const auto& name = resource.value()[i].name();
      const std::string key = resource_key_prefix + std::to_string(i);
      const std::string value = name.has_value() ? name.value() : "unknown";
      props.emplace_back(fdf::MakeProperty2(key, value));
    }
  };
  add_props(node_.mmio(), bind_fuchsia_resource::MMIO_COUNT, "fuchsia.resource.MMIO_");
  add_props(node_.irq(), bind_fuchsia_resource::INTERRUPT_COUNT, "fuchsia.resource.INTERRUPT_");
  add_props(node_.bti(), bind_fuchsia_resource::BTI_COUNT, "fuchsia.resource.BTI_");
  add_props(node_.smc(), bind_fuchsia_resource::SMC_COUNT, "fuchsia.resource.SMC_");

  std::vector offers = {
      fdf::MakeOffer2<fuchsia_hardware_platform_device::Service>(name_),
      fdf::MakeOffer2<fuchsia_driver_compat::Service>(name_),
      serial_number_metadata_server_.MakeOffer(),
      partition_map_metadata_server_.MakeOffer(),
      mac_address_metadata_server_.MakeOffer(),
  };

  // Add our offers to the outgoing directory
  {
    zx::result result = bus()->outgoing()->AddService<fuchsia_hardware_platform_device::Service>(
        fuchsia_hardware_platform_device::Service::InstanceHandler({
            .device = device_bindings_.CreateHandler(
                this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                fidl::kIgnoreBindingClosure),
        }),
        name_);
    if (result.is_error()) {
      fdf::warn("Failed to add platform device service: {}", result);
      return result.take_error();
    }
  }

  // Setup boot metadata servers.
  if (zx::result result =
          serial_number_metadata_server_.Serve(*bus()->outgoing(), bus()->dispatcher());
      result.is_error()) {
    fdf::error("Failed to serve serial number metadata server: {}", result);
    return result.take_error();
  }

  if (zx::result result =
          partition_map_metadata_server_.Serve(*bus()->outgoing(), bus()->dispatcher());
      result.is_error()) {
    fdf::error("Failed to serve partition map metadata server: {}", result);
    return result.take_error();
  }

  if (zx::result result =
          mac_address_metadata_server_.Serve(*bus()->outgoing(), bus()->dispatcher());
      result.is_error()) {
    fdf::error("Failed to serve mac address metadata server: {}", result);
    return result.take_error();
  }

  auto [client, server] = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  fuchsia_driver_framework::NodeAddArgs args{{
      .name = {name},
      .offers2 = std::move(offers),
      .bus_info = std::move(bus_info),
      .properties2 = std::move(props),
  }};

  auto parent = bus()->platform_node();
  fidl::Result result = fidl::Call(parent)->AddChild({std::move(args), std::move(server), {}});

  if (result.is_error()) {
    fdf::error("Failed to add child {}. Error: {}", name, result.error_value().FormatDescription());
    return zx::error(result.error_value().is_framework_error()
                         ? result.error_value().framework_error().status()
                         : ZX_ERR_INTERNAL);
  }

  node_controller_ = std::move(std::move(client));

  return zx::ok();
}

zx::result<> PlatformDevice::Init() {
  if (node_.irq().has_value()) {
    for (uint32_t i = 0; i < node_.irq()->size(); i++) {
      auto fragment = std::make_unique<PlatformInterruptFragment>(
          this, i, fdf::Dispatcher::GetCurrent()->async_dispatcher());
      auto name = std::format("{}-irq{:03}", name_, i);
      zx::result result = fragment->Add(name.c_str(), this, node_.irq().value()[i]);
      if (result.is_error()) {
        fdf::warn("Failed to create interrupt fragment {}", i);
        continue;
      }

      fragments_.push_back(std::move(fragment));
    }
  }

  device_server_.Init(name_);
  device_server_.Serve(bus()->dispatcher(), bus()->outgoing().get());

  inspect_node_.RecordLazyValues("interrupt_vectors",
                                 fit::bind_member<&PlatformDevice::InspectNodeCallback>(this));

  const size_t metadata_count = node_.metadata() == std::nullopt ? 0 : node_.metadata()->size();
  for (size_t i = 0; i < metadata_count; i++) {
    const auto& metadata = node_.metadata().value()[i];
    if (!IsValid(metadata)) {
      fdf::info("Metadata at index {} is invalid", i);
      return zx::error(ZX_ERR_INTERNAL);
    }

    auto metadata_id = metadata.id();
    ZX_ASSERT(metadata_id.has_value());
    auto metadata_data = metadata.data();
    ZX_ASSERT(metadata_data.has_value());

    // TODO(b/341981272): Remove `device_server.AddMetadata()` once all drivers bound to platform
    // devices do not use `device_get_metadata()` to retrieve metadata. They should be using
    // fuchsia.hardware.platform.device/Device::GetMetadata().
    errno = 0;
    char* metadata_id_end{};
    const char* metadata_id_start = metadata_id.value().c_str();
    auto metadata_type =
        static_cast<uint32_t>(std::strtol(metadata_id_start, &metadata_id_end, 10));
    if (!metadata_id.value().empty() && errno == 0 && *metadata_id_end == '\0') {
      zx_status_t status =
          device_server_.AddMetadata(metadata_type, metadata_data->data(), metadata_data->size());
      if (status != ZX_OK) {
        fdf::info("Failed to add metadata with ID {}: {}", metadata_id.value().c_str(),
                  zx_status_get_string(status));
        return zx::error(status);
      }
    }

    metadata_.emplace(metadata_id.value(), metadata_data.value());
  }

  const size_t boot_metadata_count =
      node_.boot_metadata() == std::nullopt ? 0 : node_.boot_metadata()->size();
  for (size_t i = 0; i < boot_metadata_count; i++) {
    const auto& metadata = node_.boot_metadata().value()[i];
    if (!IsValid(metadata)) {
      fdf::info("Boot metadata at index {} is invalid", i);
      return zx::error(ZX_ERR_INTERNAL);
    }

    auto metadata_zbi_type = metadata.zbi_type();
    ZX_ASSERT(metadata_zbi_type.has_value());

    zx::result data =
        bus_->GetBootItemArray(metadata_zbi_type.value(), metadata.zbi_extra().value());
    if (data.is_ok()) {
      // TODO(b/341981272): Remove `device_server_.AddMetadata()` once all drivers bound to platform
      // devices do not use `device_get_metadata()` to retrieve metadata.
      zx_status_t status =
          device_server_.AddMetadata(metadata_zbi_type.value(), data->data(), data->size());
      if (status != ZX_OK) {
        fdf::warn("Failed to add boot metadata with ZBI type {}: {}", metadata_zbi_type.value(),
                  zx_status_get_string(status));
      }

      metadata_.emplace(std::to_string(metadata_zbi_type.value()),
                        std::vector<uint8_t>{data->begin(), data->end()});

      switch (metadata_zbi_type.value()) {
        case ZBI_TYPE_SERIAL_NUMBER: {
          auto metadata = CreateSerialNumberMetadata(data.value());
          if (zx::result result = serial_number_metadata_server_.SetMetadata(metadata);
              result.is_error()) {
            fdf::error("Failed to set metadata for serial number metadata server: {}", result);
            return result.take_error();
          }
          break;
        }
        case ZBI_TYPE_DRV_PARTITION_MAP: {
          zx::result metadata = CreatePartitionMapMetadata(data.value());
          if (metadata.is_error()) {
            fdf::error("Failed to create partition map metadata: {}", metadata);
            return metadata.take_error();
          }
          if (zx::result result = partition_map_metadata_server_.SetMetadata(metadata.value());
              result.is_error()) {
            fdf::error("Failed to set metadata for partition map metadata server: {}", result);
            return result.take_error();
          }
          break;
        }
        case ZBI_TYPE_DRV_MAC_ADDRESS: {
          zx::result metadata = CreateMacAddressMetadata(data.value());
          if (metadata.is_error()) {
            fdf::error("Failed to create mac address metadata: {}", metadata);
            return metadata.take_error();
          }
          if (zx::result result = mac_address_metadata_server_.SetMetadata(metadata.value());
              result.is_error()) {
            fdf::error("Failed to set metadata for mac address metadata server: {}", result);
            return result.take_error();
          }
          break;
        }
        default:
          fdf::info("Ignoring boot metadata with zbi type {}", metadata_zbi_type.value());
          break;
      }
    }
  }

  return zx::ok();
}

void PlatformDevice::GetMmioById(GetMmioByIdRequestView request,
                                 GetMmioByIdCompleter::Sync& completer) {
  zx::result mmio_result = GetMmio(request->index);
  if (mmio_result.is_error()) {
    completer.ReplyError(mmio_result.status_value());
    return;
  }

  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                             .offset(mmio_result->offset)
                             .size(mmio_result->size)
                             .vmo(zx::vmo(mmio_result->vmo))
                             .Build());
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

  zx::result mmio_result = GetMmio(index.value());
  if (mmio_result.is_error()) {
    completer.ReplyError(mmio_result.status_value());
    return;
  }

  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::Mmio::Builder(arena)
                             .offset(mmio_result->offset)
                             .size(mmio_result->size)
                             .vmo(zx::vmo(mmio_result->vmo))
                             .Build());
}

void PlatformDevice::GetInterruptById(GetInterruptByIdRequestView request,
                                      GetInterruptByIdCompleter::Sync& completer) {
  zx::result interrupt = GetInterrupt(request->index, request->flags);
  if (interrupt.is_ok()) {
    completer.ReplySuccess(std::move(interrupt.value()));
  } else {
    completer.ReplyError(interrupt.status_value());
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
  zx::result interrupt = GetInterrupt(index.value(), request->flags);
  if (interrupt.is_ok()) {
    completer.ReplySuccess(std::move(interrupt.value()));
  } else {
    completer.ReplyError(interrupt.status_value());
  }
}

void PlatformDevice::GetBtiById(GetBtiByIdRequestView request,
                                GetBtiByIdCompleter::Sync& completer) {
  zx::result bti = GetBti(request->index);
  if (bti.is_ok()) {
    completer.ReplySuccess(std::move(bti.value()));
  } else {
    completer.ReplyError(bti.status_value());
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
  zx::result bti = GetBti(index.value());
  if (bti.is_ok()) {
    completer.ReplySuccess(std::move(bti.value()));
  } else {
    completer.ReplyError(bti.status_value());
  }
}

void PlatformDevice::GetSmcById(GetSmcByIdRequestView request,
                                GetSmcByIdCompleter::Sync& completer) {
  zx::result smc = GetSmc(request->index);
  if (smc.is_ok()) {
    completer.ReplySuccess(std::move(smc.value()));
  } else {
    completer.ReplyError(smc.status_value());
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
  zx::result smc = GetSmc(index.value());
  if (smc.is_ok()) {
    completer.ReplySuccess(std::move(smc.value()));
  } else {
    completer.ReplyError(smc.status_value());
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
  DeviceInfo device_info = GetDeviceInfo();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::NodeDeviceInfo::Builder(arena)
                             .vid(device_info.vid)
                             .pid(device_info.pid)
                             .did(device_info.did)
                             .mmio_count(device_info.mmio_count)
                             .irq_count(device_info.irq_count)
                             .bti_count(device_info.bti_count)
                             .smc_count(device_info.smc_count)
                             .metadata_count(device_info.metadata_count)
                             .name(device_info.name)
                             .Build());
}

void PlatformDevice::GetBoardInfo(GetBoardInfoCompleter::Sync& completer) {
  BoardInfo board_info = GetBoardInfo();
  fidl::Arena arena;
  completer.ReplySuccess(fuchsia_hardware_platform_device::wire::BoardInfo::Builder(arena)
                             .vid(board_info.vid)
                             .pid(board_info.pid)
                             .board_name(board_info.board_name)
                             .board_revision(board_info.board_revision)
                             .Build());
}

void PlatformDevice::GetMetadata(GetMetadataRequestView request,
                                 GetMetadataCompleter::Sync& completer) {
  if (auto metadata = metadata_.find(request->id.get()); metadata != metadata_.end()) {
    completer.ReplySuccess(fidl::VectorView<uint8_t>::FromExternal(metadata->second));
    return;
  }

  completer.ReplyError(ZX_ERR_NOT_FOUND);
}

void PlatformDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  fdf::warn("PlatformDevice received unknown method with ordinal: {}", metadata.method_ordinal);
}

}  // namespace platform_bus

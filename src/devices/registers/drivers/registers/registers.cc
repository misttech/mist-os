// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "registers.h"

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <string>

#include <bind/fuchsia/register/cpp/bind.h>
#include <fbl/auto_lock.h>

namespace registers {

namespace {

template <typename T>
T GetMask(const fuchsia_hardware_registers::wire::Mask& mask);

template <>
uint8_t GetMask(const fuchsia_hardware_registers::wire::Mask& mask) {
  return static_cast<uint8_t>(mask.r8());
}
template <>
uint16_t GetMask(const fuchsia_hardware_registers::wire::Mask& mask) {
  return static_cast<uint16_t>(mask.r16());
}
template <>
uint32_t GetMask(const fuchsia_hardware_registers::wire::Mask& mask) {
  return static_cast<uint32_t>(mask.r32());
}
template <>
uint64_t GetMask(const fuchsia_hardware_registers::wire::Mask& mask) {
  return static_cast<uint64_t>(mask.r64());
}

zx::result<std::vector<uint8_t>> ParseMetadata(
    const fidl::VectorView<fuchsia_driver_compat::wire::Metadata>& metadata) {
  for (const auto& m : metadata) {
    if (m.type == DEVICE_METADATA_REGISTERS) {
      size_t size;
      auto status = m.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to get_prop_content_size %s", zx_status_get_string(status));
        continue;
      }

      std::vector<uint8_t> metadata;
      metadata.resize(size);
      status = m.data.read(metadata.data(), 0, metadata.size());
      if (status != ZX_OK) {
        FDF_LOG(ERROR, "Failed to read %s", zx_status_get_string(status));
        continue;
      }

      return zx::ok(std::move(metadata));
    }
  }

  FDF_LOG(ERROR, "Failed to parse metadata.");
  return zx::error(ZX_ERR_NOT_FOUND);
}

template <typename T>
zx::result<> CheckOverlappingBits(
    const fidl::ObjectView<fuchsia_hardware_registers::wire::Metadata>& metadata,
    const std::map<uint32_t, std::shared_ptr<MmioInfo>>& mmios) {
  std::map<uint32_t, std::map<size_t, T>> overlap;
  for (const auto& reg : metadata->registers()) {
    if (!reg.has_name() || !reg.has_mmio_id() || !reg.has_masks()) {
      // Doesn't have to have all Register IDs.
      continue;
    }

    if (mmios.find(reg.mmio_id()) == mmios.end()) {
      FDF_LOG(ERROR, "%s: Invalid MMIO ID %u for Register %.*s.\n", __func__, reg.mmio_id(),
              static_cast<int>(reg.name().size()), reg.name().data());
      return zx::error(ZX_ERR_INTERNAL);
    }

    for (const auto& m : reg.masks()) {
      if (m.mmio_offset() / sizeof(T) >= mmios.at(reg.mmio_id())->locks_.size()) {
        FDF_LOG(ERROR, "%s: Invalid offset.\n", __func__);
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (!m.overlap_check_on()) {
        continue;
      }

      if (overlap.find(reg.mmio_id()) == overlap.end()) {
        overlap[reg.mmio_id()] = {};
      }
      if (overlap[reg.mmio_id()].find(m.mmio_offset() / sizeof(T)) ==
          overlap[reg.mmio_id()].end()) {
        overlap[reg.mmio_id()][m.mmio_offset() / sizeof(T)] = 0;
      }

      auto& bits = overlap[reg.mmio_id()][m.mmio_offset() / sizeof(T)];
      auto mask = GetMask<T>(m.mask());
      if (bits & mask) {
        FDF_LOG(ERROR, "%s: Overlapping bits in MMIO ID %u, Register No. %lu, Bit mask 0x%lx\n",
                __func__, reg.mmio_id(), m.mmio_offset() / sizeof(T),
                static_cast<uint64_t>(bits & mask));
        return zx::error(ZX_ERR_INTERNAL);
      }
      bits |= mask;
    }
  }

  return zx::ok();
}

zx::result<> ValidateMetadata(
    const fidl::ObjectView<fuchsia_hardware_registers::wire::Metadata>& metadata,
    const std::map<uint32_t, std::shared_ptr<MmioInfo>>& mmios) {
  if (!metadata->has_registers()) {
    FDF_LOG(ERROR, "Metadata incomplete");
    return zx::error(ZX_ERR_INTERNAL);
  }
  bool begin = true;
  fuchsia_hardware_registers::wire::Mask::Tag tag;
  for (const auto& reg : metadata->registers()) {
    if (!reg.has_name() || !reg.has_mmio_id() || !reg.has_masks()) {
      FDF_LOG(ERROR, "Metadata incomplete");
      return zx::error(ZX_ERR_INTERNAL);
    }

    if (begin) {
      tag = reg.masks().begin()->mask().Which();
      begin = false;
    }

    for (const auto& mask : reg.masks()) {
      if (!mask.has_mask() || !mask.has_mmio_offset() || !mask.has_count()) {
        FDF_LOG(ERROR, "Metadata incomplete");
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (mask.mask().Which() != tag) {
        FDF_LOG(ERROR, "Width of registers don't match up.");
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (mask.mmio_offset() % SWITCH_BY_TAG(tag, GetSize)) {
        FDF_LOG(ERROR, "%s: Mask with offset 0x%08lx is not aligned", __func__, mask.mmio_offset());
        return zx::error(ZX_ERR_INTERNAL);
      }
    }
  }

  return SWITCH_BY_TAG(tag, CheckOverlappingBits, metadata, mmios);
}

}  // namespace

template <typename T>
zx::result<> RegistersDevice::CreateNode(Register<T>& reg) {
  auto result =
      outgoing()->AddService<fuchsia_hardware_registers::Service>(reg.GetHandler(), reg.id());
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add service to the outgoing directory");
    return result.take_error();
  }

  // Initialize our compat server.
  {
    zx::result result =
        reg.compat_server_.Initialize(incoming(), outgoing(), node_name(), reg.id());
    if (result.is_error()) {
      return result.take_error();
    }
  }

  fidl::Arena arena;
  auto offers = reg.compat_server_.CreateOffers2(arena);
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_registers::Service>(arena, reg.id()));
  auto properties = std::vector{
      fdf::MakeProperty(arena, bind_fuchsia_register::NAME, reg.id()),
  };
  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, "register-" + reg.id())
                  .offers2(arena, std::move(offers))
                  .properties(arena, std::move(properties))
                  .Build();

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Failed to create controller endpoints: %s",
                controller_endpoints.status_string());
  {
    fidl::WireResult result =
        fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints->server), {});
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add child %s", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
  }
  reg.controller_.Bind(std::move(controller_endpoints->client));

  return zx::ok();
}

template <typename T>
zx::result<> RegistersDevice::Create(
    fuchsia_hardware_registers::wire::RegistersMetadataEntry& reg) {
  if (!reg.has_name() || !reg.has_mmio_id() || !reg.has_masks()) {
    // Doesn't have to have all Register IDs.
    return zx::error(ZX_ERR_BAD_STATE);
  }

  std::map<uint64_t, std::pair<T, uint32_t>> masks;
  for (const auto& m : reg.masks()) {
    auto mask = GetMask<T>(m.mask());
    masks.emplace(m.mmio_offset(), std::make_pair(mask, m.count()));
  }
  return std::visit(
      [&](auto&& d) { return CreateNode(d); },
      registers_.emplace_back(std::in_place_type<Register<T>>, mmios_[reg.mmio_id()],
                              std::string(reg.name().data(), reg.name().size()), std::move(masks)));
}

zx::result<> RegistersDevice::MapMmio(fuchsia_hardware_registers::wire::Mask::Tag& tag) {
  zx::result result = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to open pdev service: %s", result.status_string());
    return result.take_error();
  }
  fidl::WireSyncClient pdev(std::move(result.value()));
  if (!pdev.is_valid()) {
    FDF_LOG(ERROR, "Failed to get pdev");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  auto device_info = pdev->GetNodeDeviceInfo();
  if (!device_info.ok() || device_info->is_error()) {
    FDF_LOG(ERROR, "Could not get device info %s", device_info.FormatDescription().c_str());
    return zx::error(device_info.ok() ? device_info->error_value() : device_info.error().status());
  }

  ZX_ASSERT(device_info->value()->has_mmio_count());
  for (uint32_t i = 0; i < device_info->value()->mmio_count(); i++) {
    auto mmio = pdev->GetMmioById(i);
    if (!mmio.ok() || mmio->is_error()) {
      FDF_LOG(ERROR, "Could not get mmio regions %s", mmio.FormatDescription().c_str());
      return zx::error(mmio.ok() ? mmio->error_value() : mmio.error().status());
    }

    if (!mmio->value()->has_vmo() || !mmio->value()->has_size() || !mmio->value()->has_offset()) {
      FDF_LOG(ERROR, "GetMmioById(%d) returned invalid MMIO", i);
      return zx::error(ZX_ERR_BAD_STATE);
    }

    zx::result mmio_buffer =
        fdf::MmioBuffer::Create(mmio->value()->offset(), mmio->value()->size(),
                                std::move(mmio->value()->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
    if (mmio_buffer.is_error()) {
      FDF_LOG(ERROR, "Failed to map MMIO: %s", mmio_buffer.status_string());
      return zx::error(mmio_buffer.error_value());
    }

    zx::result<MmioInfo> mmio_info = SWITCH_BY_TAG(tag, MmioInfo::Create, std::move(*mmio_buffer));
    if (mmio_info.is_error()) {
      FDF_LOG(ERROR, "Could not create mmio info %d", mmio_info.error_value());
      return zx::error(mmio_info.take_error());
    }

    mmios_.emplace(i, std::make_shared<MmioInfo>(std::move(*mmio_info)));
  }

  return zx::ok();
}

zx::result<> RegistersDevice::Start() {
  // Get metadata.
  std::vector<uint8_t> metadata;
  {
    zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to open compat service: %s", result.status_string());
      return result.take_error();
    }
    auto compat = fidl::WireSyncClient(std::move(result.value()));
    if (!compat.is_valid()) {
      FDF_LOG(ERROR, "Failed to get compat");
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    auto data = compat->GetMetadata();
    if (!data.ok()) {
      FDF_LOG(ERROR, "Failed to GetMetadata %s", data.error().FormatDescription().c_str());
      return zx::error(data.error().status());
    }
    if (data->is_error()) {
      FDF_LOG(ERROR, "Failed to GetMetadata %s", zx_status_get_string(data->error_value()));
      return data->take_error();
    }

    auto vals = ParseMetadata(data.value()->metadata);
    if (vals.is_error()) {
      FDF_LOG(ERROR, "Failed to ParseMetadata %s", zx_status_get_string(vals.error_value()));
      return vals.take_error();
    }
    metadata = std::move(vals.value());
  }
  auto parsed_metadata =
      fidl::InplaceUnpersist<fuchsia_hardware_registers::wire::Metadata>(cpp20::span(metadata));
  if (parsed_metadata.is_error()) {
    FDF_LOG(ERROR, "InplaceUnpersist failed %s",
            parsed_metadata.error_value().FormatDescription().c_str());
    return zx::error(parsed_metadata.error_value().status());
  }
  auto tag = parsed_metadata->registers()[0].masks()[0].mask().Which();

  // Get mmio.
  {
    auto result = MapMmio(tag);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to map MMIOs %d", result.error_value());
      return result.take_error();
    }
  }

  // Validate metadata.
  {
    auto result = ValidateMetadata(parsed_metadata.value(), mmios_);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to validate metadata %d", result.error_value());
      return result.take_error();
    }
  }

  // Create devices.
  for (auto& reg : parsed_metadata->registers()) {
    auto result = SWITCH_BY_TAG(tag, Create, reg);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create device for %s %d",
              reg.has_name() ? reg.name().data() : "Unknown", result.error_value());
    }
  }

  return zx::ok();
}

}  // namespace registers

FUCHSIA_DRIVER_EXPORT(registers::RegistersDevice);

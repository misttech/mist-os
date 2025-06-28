// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "registers.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/platform-device/cpp/pdev.h>

#include <string>

#include <bind/fuchsia/register/cpp/bind.h>
#include <fbl/auto_lock.h>

namespace registers {

namespace {

template <typename T>
T GetMask(const fuchsia_hardware_registers::Mask& mask);

template <>
uint8_t GetMask(const fuchsia_hardware_registers::Mask& mask) {
  return static_cast<uint8_t>(mask.r8().value());
}
template <>
uint16_t GetMask(const fuchsia_hardware_registers::Mask& mask) {
  return static_cast<uint16_t>(mask.r16().value());
}
template <>
uint32_t GetMask(const fuchsia_hardware_registers::Mask& mask) {
  return static_cast<uint32_t>(mask.r32().value());
}
template <>
uint64_t GetMask(const fuchsia_hardware_registers::Mask& mask) {
  return static_cast<uint64_t>(mask.r64().value());
}

template <typename T>
zx::result<> CheckOverlappingBits(const fuchsia_hardware_registers::Metadata& metadata,
                                  const std::map<uint32_t, std::shared_ptr<MmioInfo>>& mmios) {
  std::map<uint32_t, std::map<size_t, T>> overlap;
  for (const auto& reg : metadata.registers().value()) {
    if (!reg.name().has_value() || !reg.mmio_id().has_value() || !reg.masks().has_value()) {
      // Doesn't have to have all Register IDs.
      continue;
    }

    auto mmio_id = reg.mmio_id().value();
    if (!mmios.contains(mmio_id)) {
      FDF_LOG(ERROR, "Invalid MMIO ID %u for Register %s", mmio_id, reg.name().value().c_str());
      return zx::error(ZX_ERR_INTERNAL);
    }

    for (const auto& m : reg.masks().value()) {
      auto mmio_offset = m.mmio_offset().value();
      if (mmio_offset / sizeof(T) >= mmios.at(mmio_id)->locks_.size()) {
        FDF_LOG(ERROR, "Invalid offset");
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (!m.mask().has_value()) {
        FDF_LOG(ERROR, "Makse missing mask property");
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      if (!m.overlap_check_on()) {
        continue;
      }

      if (overlap.find(mmio_id) == overlap.end()) {
        overlap[mmio_id] = {};
      }

      for (int i = 0; i < m.count(); i++) {
        auto idx = mmio_offset / sizeof(T) + i;
        if (overlap[mmio_id].find(idx) == overlap[mmio_id].end()) {
          overlap[mmio_id][idx] = 0;
        }

        auto& bits = overlap[mmio_id][idx];
        auto mask = GetMask<T>(m.mask().value());
        if (bits & mask) {
          FDF_LOG(ERROR, "Overlapping bits in MMIO ID %u, Register No. %lu, Bit mask 0x%lx",
                  mmio_id, idx, static_cast<uint64_t>(bits & mask));
          return zx::error(ZX_ERR_INTERNAL);
        }
        bits |= mask;
      }
    }
  }

  return zx::ok();
}

zx::result<> ValidateMetadata(const fuchsia_hardware_registers::Metadata& metadata,
                              const std::map<uint32_t, std::shared_ptr<MmioInfo>>& mmios) {
  if (!metadata.registers().has_value()) {
    FDF_LOG(ERROR, "Metadata missing registers field");
    return zx::error(ZX_ERR_INTERNAL);
  }
  bool begin = true;
  fuchsia_hardware_registers::Mask::Tag tag;
  const auto& registers = metadata.registers().value();
  for (size_t i = 0; i < registers.size(); ++i) {
    const auto& reg = registers[i];
    if (!reg.name().has_value()) {
      FDF_LOG(ERROR, "Register %lu missing name field", i);
      return zx::error(ZX_ERR_INTERNAL);
    }
    if (!reg.mmio_id().has_value()) {
      FDF_LOG(ERROR, "Register %lu missing mmio_id field", i);
      return zx::error(ZX_ERR_INTERNAL);
    }
    if (!reg.masks().has_value()) {
      FDF_LOG(ERROR, "Register %lu missing masks field", i);
      return zx::error(ZX_ERR_INTERNAL);
    }

    const auto& masks = reg.masks().value();
    if (begin) {
      tag = masks.begin()->mask().value().Which();
      begin = false;
    }

    for (size_t j = 0; j < masks.size(); ++j) {
      const auto& mask = masks[j];
      if (!mask.mask().has_value()) {
        FDF_LOG(ERROR, "Mask %lu of register %lu missing mask field", j, i);
        return zx::error(ZX_ERR_INTERNAL);
      }
      if (!mask.mmio_offset().has_value()) {
        FDF_LOG(ERROR, "Mask %lu of register %lu missing mmio_offset field", j, i);
        return zx::error(ZX_ERR_INTERNAL);
      }
      if (!mask.count().has_value()) {
        FDF_LOG(ERROR, "Mask %lu of register %lu missing count field", j, i);
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (mask.mask().value().Which() != tag) {
        FDF_LOG(ERROR, "Width of registers don't match up.");
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (mask.mmio_offset().value() % SWITCH_BY_TAG(tag, GetSize)) {
        FDF_LOG(ERROR, "Mask with offset 0x%08lx is not aligned", mask.mmio_offset().value());
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

  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  {
    fidl::WireResult result =
        fidl::WireCall(node())->AddChild(args, std::move(controller_endpoints.server), {});
    if (!result.ok()) {
      FDF_LOG(ERROR, "Failed to add child %s", result.FormatDescription().c_str());
      return zx::error(result.status());
    }
  }
  reg.controller_.Bind(std::move(controller_endpoints.client));

  return zx::ok();
}

template <typename T>
zx::result<> RegistersDevice::Create(
    const fuchsia_hardware_registers::RegistersMetadataEntry& reg) {
  if (!reg.name().has_value()) {
    FDF_LOG(ERROR, "Register missing name field");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (!reg.mmio_id().has_value()) {
    FDF_LOG(ERROR, "Register missing mmio_id field");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (!reg.masks().has_value()) {
    FDF_LOG(ERROR, "Register missing masks field");
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::map<uint64_t, std::pair<T, uint32_t>> masks;
  for (const auto& m : reg.masks().value()) {
    auto mask = GetMask<T>(m.mask().value());
    masks.emplace(m.mmio_offset().value(), std::make_pair(mask, m.count().value()));
  }
  return std::visit(
      [&](auto&& d) { return CreateNode(d); },
      registers_.emplace_back(std::in_place_type<Register<T>>, mmios_[reg.mmio_id().value()],
                              std::string(reg.name().value()), std::move(masks)));
}

zx::result<> RegistersDevice::MapMmio(fuchsia_hardware_registers::Mask::Tag& tag) {
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
  zx::result pdev_client = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
    return pdev_client.take_error();
  }
  fdf::PDev pdev{std::move(pdev_client.value())};
  zx::result metadata_result = pdev.GetFidlMetadata<fuchsia_hardware_registers::Metadata>();
  if (metadata_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get metadata: %s", metadata_result.status_string());
    return metadata_result.take_error();
  }
  const auto& metadata = metadata_result.value();
  auto tag = metadata.registers().value()[0].masks().value()[0].mask().value().Which();

  // Get mmio.
  {
    auto result = MapMmio(tag);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to map MMIOs: %s", result.status_string());
      return result.take_error();
    }
  }

  // Validate metadata.
  {
    auto result = ValidateMetadata(metadata, mmios_);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to validate metadata: %s", result.status_string());
      return result.take_error();
    }
  }

  // Create devices.
  for (const auto& reg : metadata.registers().value()) {
    auto result = SWITCH_BY_TAG(tag, Create, reg);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create device for %s: %s", reg.name().value().c_str(),
              result.status_string());
    }
  }

  return zx::ok();
}

}  // namespace registers

FUCHSIA_DRIVER_EXPORT(registers::RegistersDevice);

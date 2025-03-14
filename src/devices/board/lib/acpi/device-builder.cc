// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/lib/acpi/device-builder.h"

#include <trace.h>
#include <zircon/compiler.h>

#include "src/devices/board/lib/acpi/device-args.h"
#include "src/devices/board/lib/acpi/manager.h"
#include "src/devices/board/lib/acpi/resources.h"
#include "src/devices/lib/acpi/util.h"

#define LOCAL_TRACE 0

// Define operator new/delete for the kernel context
void* operator new(unsigned long size) { return malloc(size); }

namespace acpi {

acpi::status<> DeviceBuilder::GatherResources(acpi::Acpi* acpi, acpi::Manager* manager,
                                              GatherResourcesCallback callback) {
  if (!handle_ || !parent_) {
    // Skip the root device.
    return acpi::ok();
  }

  // Don't decode resources if the ENABLED bit is not set.
  // See ACPI v6.4 section 6.3.7
  if (!(state_ & ACPI_STA_DEVICE_ENABLED)) {
    return acpi::ok();
  }

  // TODO(https://fxbug.dev/42158705): Handle other resources like serial buses.
  auto result = acpi->WalkResources(
      handle_, "_CRS", [this, acpi, manager, &callback](ACPI_RESOURCE* res) -> acpi::status<> {
        ACPI_HANDLE bus_parent = nullptr;
        BusType type = BusType::kUnknown;
        DeviceChildEntry entry;
        // const char* bus_id_prop;
        if (resource_is_spi(res)) {
          type = BusType::kSpi;
          auto result = resource_parse_spi(acpi, handle_, res, &bus_parent);
          if (result.is_error()) {
            LTRACEF("Failed to parse SPI resource: %d\n", result.error_value());
            return result.take_error();
          }
          // entry = result.value();
          // bus_id_prop = bind_fuchsia::SPI_BUS_ID.c_str();
          // str_props_.emplace_back(
          //     OwnedStringProp(bind_fuchsia::SPI_CHIP_SELECT.c_str(),
          //     result.value().cs().value()));
        } else if (resource_is_i2c(res)) {
          type = BusType::kI2c;
          auto result = resource_parse_i2c(acpi, handle_, res, &bus_parent);
          if (result.is_error()) {
            LTRACEF("Failed to parse I2C resource: %d\n", result.error_value());
            return result.take_error();
          }
          // entry = result.value();
          // bus_id_prop = bind_fuchsia::I2C_BUS_ID.c_str();
          //;
          // str_props_.emplace_back(
          //  OwnedStringProp(bind_fuchsia::I2C_ADDRESS.c_str(), result.value().address().value()));
        } else if (resource_is_irq(res)) {
          irq_count_++;
        }

        if (bus_parent) {
          size_t bus_index = callback(bus_parent, type, entry);
          DeviceBuilder* b = manager->LookupDevice(bus_parent);
          fbl::AllocChecker ac;
          buses_.push_back(ktl::move(std::pair<DeviceBuilder*, size_t>(b, bus_index)), &ac);
          ZX_ASSERT(ac.check());
          // str_props_.emplace_back(OwnedStringProp(bus_id_prop, b->GetBusId()));
          has_address_ = true;
        }
        return acpi::ok();
      });

  if (result.is_error() && result.zx_status_value() != ZX_ERR_NOT_FOUND) {
    return result.take_error();
  }

  auto info = acpi->GetObjectInfo(handle_);
  if (info.is_error()) {
    LTRACEF("Failed to get object info: %d\n", info.status_value());
    return info.take_error();
  }

  // PCI is special, and PCI devices don't have an explicit resource. Instead, we need to check
  // _ADR for PCI addressing info.
  if (parent_->bus_type_ == BusType::kPci) {
    if (info->Valid & ACPI_VALID_ADR) {
      callback(parent_->handle_, BusType::kPci, DeviceChildEntry(info->Address));
      // Set up some bind properties for ourselves. callback() should set HasBusId.
      ZX_ASSERT(parent_->HasBusId());
      // uint32_t bus_id = parent_->GetBusId();
      // uint32_t device = (info->Address & (0xffff0000)) >> 16;
      // uint32_t func = info->Address & 0x0000ffff;
      /*str_props_.emplace_back(OwnedStringProp(bind_fuchsia::PCI_TOPO.c_str(),
                                              BIND_PCI_TOPO_PACK(bus_id, device, func)));*/
      // Should we buses_.emplace_back() here? The PCI bus driver currently publishes PCI
      // composites, so having a device on a PCI bus that uses other buses resources can't be
      // represented. Such devices don't seem to exist, but if we ever encounter one, it will need
      // to be handled somehow.
      has_address_ = true;
    }
  }

  bool has_devicetree_cid = false;
  // Add HID and CID properties, if present.
  if (info->Valid & ACPI_VALID_HID) {
    if (!strcmp(info->HardwareId.String, kDeviceTreeLinkID)) {
      has_devicetree_cid = CheckForDeviceTreeCompatible(acpi);
    } else {
      /*str_props_.emplace_back(
          OwnedStringProp(bind_fuchsia_acpi::HID.c_str(), info->HardwareId.String));*/
    }
  }

  if (!has_devicetree_cid && info->Valid & ACPI_VALID_CID && info->CompatibleIdList.Count > 0) {
    auto& first = info->CompatibleIdList.Ids[0];
    if (!strcmp(first.String, kDeviceTreeLinkID)) {
      has_devicetree_cid = CheckForDeviceTreeCompatible(acpi);
    } else {
      // We only expose the first CID.
      // str_props_.emplace_back(OwnedStringProp(bind_fuchsia_acpi::FIRST_CID.c_str(),
      // first.String));
    }
  }

  // If our parent has a bus type, and we have an address on that bus, then we'll expose it in our
  // bind properties.
  if (parent_->GetBusType() != BusType::kUnknown && has_address_) {
    // str_props_.emplace_back(
    // OwnedStringProp(bind_fuchsia::ACPI_BUS_TYPE.c_str(), parent_->GetBusType()));
  }
  if (result.status_value() == AE_NOT_FOUND) {
    return acpi::ok();
  }
  return result;
}

zx::result<> DeviceBuilder::Build(acpi::Manager* manager) {
  DeviceArgs device_args(manager, handle_);
  if (HasBusId() && bus_type_ != BusType::kPci) {
    zx::result metadata = GetMetadata();
    if (metadata.is_error()) {
      LTRACEF("Failed to get metadata for '%s': %d\n", name(), metadata.status_value());
      return metadata.take_error();
    }
    device_args.SetBusMetadata(std::move(*metadata), bus_type_, GetBusId());
  }
  // auto device = std::make_unique<Device>(std::move(device_args));

  // Narrow our custom type down to zx_device_str_prop_t.
  // Any strings in zx_device_str_prop_t will still point at their equivalents
  // in the original str_props_ array.
  // std::vector<zx_device_str_prop_t> str_props_for_ddkadd;
  // for (auto& str_prop : str_props_) {
  //  str_props_for_ddkadd.emplace_back(str_prop);
  //}

  // uint32_t add_flags = DEVICE_ADD_MUST_ISOLATE;
  // if ((state_ & (ACPI_STA_DEVICE_FUNCTIONING | ACPI_STA_DEVICE_PRESENT)) ==
  //     ACPI_STA_DEVICE_FUNCTIONING) {
  //   // Don't bind drivers to this device if it is functioning but not present.
  //   // See ACPI 6.4 section 6.3.7.
  //   add_flags |= DEVICE_ADD_NON_BINDABLE;
  // }

  // auto result = device->AddDevice(name(), cpp20::span(str_props_for_ddkadd), add_flags);
  // if (result.is_error()) {
  //   LTRACEF("failed to publish acpi device '%s' (parent=%s): %d\n", name(), parent_->name(),
  //           result.status_value());
  //   return result.take_error();
  // }

  // auto* acpi_dev = device.release();

  for (uint32_t i = 0; i < irq_count_; i++) {
    /*auto result = IrqFragment::Create(*acpi_dev, i, device_id_);
    if (result.is_error()) {
      LTRACEF("Failed to construct IRQ fragment: %d\n", result.status_value());
      return result.take_error();
    }*/
  }

  /*auto status = BuildComposite(manager, str_props_for_ddkadd, device_dispatcher);
  if (status.is_error()) {
    zxlogf(WARNING, "failed to publish composite acpi device '%s-composite': %d", name(),
           status.error_value());
    return status.take_error();
  }*/

  return zx::ok();
}

size_t DeviceBuilder::AddBusChild(DeviceChildEntry d) {
  return std::visit(
      [this](auto&& arg) {
        using T = std::decay_t<decltype(arg)>;
        // If we haven't initialised the vector yet, populate it.
        auto pval_empty = std::get_if<std::monostate>(&bus_children_);
        if (pval_empty) {
          auto tmp = DeviceChildData(fbl::Vector<T>());
          bus_children_.swap(tmp);
        }

        auto pval = std::get_if<fbl::Vector<T>>(&bus_children_);
        ZX_ASSERT_MSG(pval, "Bus %s had unexpected child type vector", name());
        fbl::AllocChecker ac;
        pval->push_back(arg, &ac);
        ZX_ASSERT(ac.check());
        return pval->size() - 1;
      },
      d);
}

zx::result<BusMetadata> DeviceBuilder::GetMetadata() {
  /*return std::visit(
      [this](DeviceChildData&& arg) -> zx::result<BusMetadata> {
        if (std::holds_alternative<std::monostate>(arg)) {
          return zx::ok(std::monostate{});
        }
        return zx::error(ZX_ERR_NOT_SUPPORTED);
      },
      bus_children_);*/
  return zx::error(ZX_ERR_NOT_SUPPORTED);
}

bool DeviceBuilder::CheckForDeviceTreeCompatible(acpi::Acpi* acpi) {
  // UUID defined in "Device Properties UUID for _DSD", Revision 2.0, Section 2.1
  // https://uefi.org/sites/default/files/resources/_DSD-device-properties-UUID.pdf
  static constexpr Uuid kDevicePropertiesUuid =
      Uuid::Create(0xdaffd814, 0x6eba, 0x4d8c, 0x8a91, 0xbc9bbf4aa301);
  auto result = acpi->EvaluateObject(handle_, "_DSD", std::nullopt);
  if (result.is_error()) {
    if (result.zx_status_value() != ZX_ERR_NOT_FOUND) {
      LTRACEF("Get _DSD for '%s' failed: %d\n", name(), result.error_value());
    }
    return false;
  }

  auto value = std::move(result.value());
  if (value->Type != ACPI_TYPE_PACKAGE) {
    LTRACEF("'%s': Badly formed _DSD return value - wrong data type\n", name());
    return false;
  }

  // The package is an array of pairs. The first item in each pair is a UUID, and the second is the
  // value of that UUID.
  ACPI_OBJECT* properties = nullptr;
  for (size_t i = 0; (i + 1) < value->Package.Count; i += 2) {
    ACPI_OBJECT* uuid_buffer = &value->Package.Elements[i];
    if (uuid_buffer->Type != ACPI_TYPE_BUFFER || uuid_buffer->Buffer.Length != acpi::kUuidBytes) {
      LTRACEF("'%s': _DSD entry %zu has invalid UUID.\n", name(), i);
      continue;
    }

    if (!memcmp(uuid_buffer->Buffer.Pointer, kDevicePropertiesUuid.bytes, acpi::kUuidBytes)) {
      properties = &value->Package.Elements[i + 1];
      break;
    }
  }

  if (!properties) {
    return false;
  }

  if (properties->Type != ACPI_TYPE_PACKAGE) {
    LTRACEF("'%s': Device Properties _DSD value is not a package.\n", name());
    return false;
  }

  // properties should be a list of packages, which are each a key/value pair.
  for (size_t i = 0; i < properties->Package.Count; i++) {
    ACPI_OBJECT* pair = &properties->Package.Elements[i];
    if (pair->Type != ACPI_TYPE_PACKAGE || pair->Package.Count != 2) {
      continue;
    }

    ACPI_OBJECT* key = &pair->Package.Elements[0];
    ACPI_OBJECT* v = &pair->Package.Elements[1];
    if (key->Type != ACPI_TYPE_STRING || key->String.Length < sizeof("compatible") - 1) {
      continue;
    }

    if (!strcmp("compatible", key->String.Pointer) && v->Type == ACPI_TYPE_STRING) {
      // str_props_.emplace_back(OwnedStringProp{"fuchsia.acpi.FIRST_CID", value->String.Pointer});
      return true;
    }
  }
  return false;
}
}  // namespace acpi

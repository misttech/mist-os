// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/board/lib/acpi/manager.h"

#include <lib/zx/result.h>
#include <trace.h>
#include <zircon/compiler.h>

#include <memory>

#include <acpica/acpi.h>

#include "src/devices/board/lib/acpi/acpi.h"
#include "src/devices/board/lib/acpi/device-args.h"
#include "src/devices/board/lib/acpi/pci.h"
#include "src/devices/board/lib/acpi/power-resource.h"
#include "src/devices/board/lib/acpi/util.h"

#define LOCAL_TRACE 2

namespace acpi {

acpi::status<> Manager::DiscoverDevices() {
  // Make sure our "ACPI root device" corresponds to the root of the ACPI tree.
  auto root = acpi_->GetHandle(nullptr, "\\");
  if (root.is_error()) {
    LTRACEF("Failed to get ACPI root object: %d\n", root.error_value());
    return root.take_error();
  }

  devices_.emplace(root.value(), DeviceBuilder::MakeRootDevice(root.value(), acpi_root_));
  uint32_t ignored_depth = Acpi::kMaxNamespaceDepth + 1;
  return acpi_->WalkNamespace(ACPI_TYPE_DEVICE, ACPI_ROOT_OBJECT, Acpi::kMaxNamespaceDepth,
                              [this, &ignored_depth](ACPI_HANDLE handle, uint32_t depth,
                                                     WalkDirection dir) -> acpi::status<> {
                                if (depth > ignored_depth) {
                                  // If we're ignoring this branch of the tree, do nothing.
                                  return acpi::ok();
                                }
                                if (dir == WalkDirection::Ascending) {
                                  if (depth == ignored_depth) {
                                    // We've come back to where we set 'ignored_depth', reset it.
                                    ignored_depth = Acpi::kMaxNamespaceDepth + 1;
                                  }
                                  // Nothing to do when ascending the tree.
                                  return acpi::ok();
                                }
                                auto status = DiscoverDevice(handle);
                                if (status.is_error()) {
                                  auto path = acpi_->GetPath(handle).value_or("(get path failed)");
                                  LTRACEF(
                                      "Failed to discover device '%s': %d. Any children will "
                                      "not be enumerated, the system may not function fully.\n",
                                      path.data(), status.error_value());
                                  // We failed to enumerate this device, so we don't enumerate any
                                  // of its children.
                                  ignored_depth = depth;
                                  return acpi::ok();
                                }
                                if (status.value()) {
                                  // This device is not present, so we should not enumerate its
                                  // children. Mark this branch of the tree as ignored.
                                  ignored_depth = depth;
                                }
                                return acpi::ok();
                              });
}

acpi::status<> Manager::ConfigureDiscoveredDevices() {
  for (auto& handle : device_publish_order_) {
    auto device = LookupDevice(handle);
    auto result = device->GatherResources(
        acpi_, this, [this](ACPI_HANDLE bus, BusType type, DeviceChildEntry child) -> size_t {
          DeviceBuilder* b = LookupDevice(bus);
          if (b == nullptr) {
            // Silently ignore.
            return -1;
          }
          b->SetBusType(type);
          size_t child_index = b->AddBusChild(child);
          if (b->HasBusId()) {
            return child_index;
          }

          // If this device is a bus, figure out its bus id.
          // For PCI buses, we us the BBN ("BIOS Bus Number") from ACPI.
          // For other buses, we simply have a counter of the number of that
          // kind of bus we've encountered.
          uint32_t bus_id;
          if (type == BusType::kPci) {
            // The first PCI bus we encounter is special.
            auto bbn_result = acpi_->CallBbn(b->handle());
            if (bbn_result.zx_status_value() == ZX_ERR_NOT_FOUND) {
              bus_id = 0;
            } else if (bbn_result.is_ok()) {
              bus_id = bbn_result.value();
            } else {
              LTRACEF("Failed to get BBN for PCI bus '%s'\n", b->name());
              return -1;
            }
          } else {
            bus_id = next_bus_ids_.emplace(type, 0).first->second++;
          }
          b->SetBusId(bus_id);
          return child_index;
        });
    if (result.is_error()) {
      LTRACEF("Failed to InferBusTypes for %s: %d\n", device->name(), result.error_value());
    }
  }

  return acpi::ok();
}

acpi::status<> Manager::PublishDevices(zx_device_t* platform_bus) {
  for (auto handle : device_publish_order_) {
    DeviceBuilder* d = LookupDevice(handle);
    if (d == nullptr) {
      continue;
    }

    auto status = d->Build(this);
    if (status.is_error()) {
      return acpi::error(AE_ERROR);
    }

    uint32_t bus_type = d->GetBusType();
    if (bus_type == BusType::kPci) {
      auto s = PublishPciBus(platform_bus, d);
      if (s.is_error()) {
        return s.take_error();
      }
    }
  }
  return acpi::ok();
}

const PowerResource* Manager::AddPowerResource(ACPI_HANDLE power_resource_handle) {
  Guard<Mutex> lock(&power_resources_lock_);
  auto power_resource_entry = power_resources_.find(power_resource_handle);
  if (power_resource_entry == power_resources_.end()) {
    PowerResource power_resource = PowerResource(acpi_, power_resource_handle);
    if (power_resource.Init() != ZX_OK) {
      return nullptr;
    }

    power_resource_entry = power_resources_.insert({power_resource_handle, power_resource}).first;
  }

  return &power_resource_entry->second;
}

zx_status_t Manager::ReferencePowerResources(
    const fbl::Vector<ACPI_HANDLE>& power_resource_handles) {
  Guard<Mutex> lock(&power_resources_lock_);

  for (auto it = power_resource_handles.begin(); it != power_resource_handles.end(); ++it) {
    auto power_resource = power_resources_.find(*it);
    ZX_ASSERT_MSG(power_resource != power_resources_.end(),
                  "Attempting to reference an unknown power resource.");
    zx_status_t status = power_resource->second.Reference();
    if (status != ZX_OK) {
      // Re-dereference all the successfully referenced power resources.
      while (it != power_resource_handles.begin()) {
        --it;
        auto pr = power_resources_.find(*it);
        pr->second.Dereference();
      }
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t Manager::DereferencePowerResources(
    const fbl::Vector<ACPI_HANDLE>& power_resource_handles) {
  Guard<Mutex> lock(&power_resources_lock_);

#if 0
  for (auto rit = power_resource_handles.rbegin(); rit != power_resource_handles.rend(); ++rit) {
    auto power_resource = power_resources_.find(*rit);
    ZX_ASSERT_MSG(power_resource != power_resources_.end(),
                  "Attempting to dereference an unknown power resource.");
    zx_status_t status = power_resource->second.Dereference();
    if (status != ZX_OK) {
      // Re-reference all the successfully dereferenced power resources.
      while (rit != power_resource_handles.rbegin()) {
        --rit;
        auto pr = power_resources_.find(*rit);
        pr->second.Reference();
      }
      return status;
    }
  }
#endif

  return ZX_OK;
}

acpi::status<bool> Manager::DiscoverDevice(ACPI_HANDLE handle) {
  auto result = acpi_->GetObjectInfo(handle);
  if (result.is_error()) {
    LTRACEF("get object info failed\n");
    return result.take_error();
  }
  UniquePtr<ACPI_DEVICE_INFO> info = std::move(result.value());
  std::string_view name(reinterpret_cast<char*>(&info->Name), sizeof(info->Name));

  // TODO(https://fxbug.dev/42160841): newer versions of ACPICA return this from GetObjectInfo().
  auto state_result = acpi_->EvaluateObject(handle, "_STA", std::nullopt);
  bool examine_children;
  uint64_t state;
  if (state_result.zx_status_value() == ZX_ERR_NOT_FOUND) {
    // No state means all bits are set.
    state = ~0;
    examine_children = true;
  } else if (state_result.is_ok()) {
    if (state_result->Type != ACPI_TYPE_INTEGER) {
      LTRACEF("%.*s returned incorrect object type for _STA\n", static_cast<int>(name.size()),
              name.data());
      return acpi::error(AE_BAD_VALUE);
    }
    state = state_result->Integer.Value;
    examine_children = state & (ACPI_STA_DEVICE_PRESENT | ACPI_STA_DEVICE_FUNCTIONING);
  } else {
    return state_result.take_error();
  }

  // See ACPI 6.4, Table 6.66.
  if (!examine_children) {
    LTRACEF("device '%.*s' not present, so not enumerating.\n", static_cast<int>(name.size()),
            name.data());
    return acpi::ok(true);
  }

  acpi::status<ACPI_HANDLE> parent = acpi::ok(handle);
  bool is_device = false;
  do {
    parent = acpi_->GetParent(*parent);
    if (parent.is_error()) {
      LTRACEF("Device '%.*s' failed to get parent: %d\n", static_cast<int>(name.size()),
              name.data(), parent.status_value());
      return parent.take_error();
    }

    auto parent_info = acpi_->GetObjectInfo(*parent);
    if (parent_info.is_error()) {
      LTRACEF("Failed to get object info: %d\n", parent_info.status_value());
      return parent_info.take_error();
    }

    is_device = parent_info->Type == ACPI_TYPE_DEVICE;

  } while (!is_device);

  DeviceBuilder* parent_ptr = LookupDevice(parent.value());
  if (parent_ptr == nullptr) {
    // Our parent should have been visited before us (since we're descending down the tree),
    // so this should never happen.
    LTRACEF("Device %.*s has no discovered parent? (%p)\n", static_cast<int>(name.size()),
            name.data(), parent.value());
    return acpi::error(AE_NOT_FOUND);
  }

  DeviceBuilder device(std::move(name), handle, parent_ptr, state, device_id_);
  device_id_++;
  if (info->Flags & ACPI_PCI_ROOT_BRIDGE) {
    device.SetBusType(BusType::kPci);
  }
  fbl::AllocChecker ac;
  device_publish_order_.push_back(handle, &ac);
  ZX_ASSERT(ac.check());
  devices_.emplace(handle, std::move(device));

  return acpi::ok(false);
}

acpi::status<> Manager::PublishPciBus(zx_device_t* platform_bus, DeviceBuilder* device) {
  if (published_pci_bus_) {
    return acpi::ok();
  }

  // Publish the PCI bus. TODO(https://fxbug.dev/42158465): we might be able to move this out of the
  // board driver when ACPI work is done. For now we do this here because we need to ensure
  // that we've published the child metadata before the PCI bus driver sees it. This also
  // means we have two devices that represent the PCI bus: one sits in the right place in the
  // ACPI topology, and the other is the actual PCI bus that sits at /dev/sys/platform/pci.
  auto info = acpi_->GetObjectInfo(device->handle());
  if (info.is_error()) {
    return info.take_error();
  }

  const DeviceChildData& children = device->GetBusChildren();
  const fbl::Vector<PciTopo>* vec_ptr = nullptr;
  if (device->HasBusChildren()) {
    vec_ptr = std::get_if<fbl::Vector<PciTopo>>(&children);
    if (!vec_ptr) {
      LTRACEF("PCI bus had non-PCI children.\n");
      return acpi::error(AE_BAD_DATA);
    }
  }
  fbl::Vector<pci_bdf_t> bdfs;
  if (vec_ptr) {
    // If we have children, generate a list of them to pass to the PCI driver.
    for (uint64_t df : *vec_ptr) {
      uint32_t dev_id = (df & (0xffff0000)) >> 16;
      uint32_t func_id = df & 0x0000ffff;

      fbl::AllocChecker ac;
      bdfs.push_back(
          pci_bdf_t{
              .bus_id = static_cast<uint8_t>(device->GetBusId()),
              .device_id = static_cast<uint8_t>(dev_id),
              .function_id = static_cast<uint8_t>(func_id),
          },
          &ac);
      ZX_ASSERT(ac.check());
    }
  }

  if (pci_init(platform_bus, device->handle(), std::move(info.value()), this, std::move(bdfs)) ==
      ZX_OK) {
    published_pci_bus_ = true;
  }
  return acpi::ok();
}

DeviceBuilder* Manager::LookupDevice(ACPI_HANDLE handle) {
  auto result = devices_.find(handle);
  if (result == devices_.end()) {
    return nullptr;
  }
  return &result->second;
}
}  // namespace acpi

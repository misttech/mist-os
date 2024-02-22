// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/inspect.h"

#include <lib/ddk/driver.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/component/cpp/service.h>

#include <utility>

#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace {
const char* BindParamName(uint32_t param_num) {
  switch (param_num) {
    case BIND_FLAGS:
      return "Flags";
    case BIND_PROTOCOL:
      return "Protocol";
    case BIND_AUTOBIND:
      return "Autobind";
    case BIND_PCI_VID:
      return "PCI.VID";
    case BIND_PCI_DID:
      return "PCI.DID";
    case BIND_PCI_CLASS:
      return "PCI.Class";
    case BIND_PCI_SUBCLASS:
      return "PCI.Subclass";
    case BIND_PCI_INTERFACE:
      return "PCI.Interface";
    case BIND_PCI_REVISION:
      return "PCI.Revision";
    case BIND_PCI_TOPO:
      return "PCI.Topology";
    case BIND_USB_VID:
      return "USB.VID";
    case BIND_USB_PID:
      return "USB.PID";
    case BIND_USB_CLASS:
      return "USB.Class";
    case BIND_USB_SUBCLASS:
      return "USB.Subclass";
    case BIND_USB_PROTOCOL:
      return "USB.Protocol";
    case BIND_PLATFORM_DEV_VID:
      return "PlatDev.VID";
    case BIND_PLATFORM_DEV_PID:
      return "PlatDev.PID";
    case BIND_PLATFORM_DEV_DID:
      return "PlatDev.DID";
    case BIND_ACPI_BUS_TYPE:
      return "ACPI.BusType";
    case BIND_IHDA_CODEC_VID:
      return "IHDA.Codec.VID";
    case BIND_IHDA_CODEC_DID:
      return "IHDA.Codec.DID";
    case BIND_IHDA_CODEC_MAJOR_REV:
      return "IHDACodec.MajorRev";
    case BIND_IHDA_CODEC_MINOR_REV:
      return "IHDACodec.MinorRev";
    case BIND_IHDA_CODEC_VENDOR_REV:
      return "IHDACodec.VendorRev";
    case BIND_IHDA_CODEC_VENDOR_STEP:
      return "IHDACodec.VendorStep";
    default:
      return NULL;
  }
}
}  // namespace

zx::result<InspectDevfs> InspectDevfs::Create(async_dispatcher_t* dispatcher) {
  InspectDevfs devfs(dispatcher);

  return zx::ok(std::move(devfs));
}

zx::result<std::string> InspectDevfs::Publish(const char* name, zx::vmo vmo) {
  if (dispatcher_ == nullptr) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::string inspect_name = "driver-" + std::to_string(InspectDevfs::inspect_dev_counter_++) +
                             (name != nullptr ? "-" + std::string{name} : "");

  // TODO(b/324637276): this is the export point for duplicated data from driver components.
  inspect::PublishVmo(dispatcher_, std::move(vmo), {.tree_name = inspect_name});

  return zx::ok(std::move(inspect_name));
}

InspectManager::InspectManager(async_dispatcher_t* dispatcher) : inspector_(dispatcher, {}) {
  zx::result devfs = InspectDevfs::Create(dispatcher);
  ZX_ASSERT(devfs.is_ok());
  info_ = fbl::MakeRefCounted<Info>(root_node(), std::move(devfs.value()));
}

DeviceInspect InspectManager::CreateDevice(std::string name, zx::vmo vmo, uint32_t protocol_id) {
  return DeviceInspect(info_, std::move(name), std::move(vmo), protocol_id);
}

DeviceInspect::DeviceInspect(fbl::RefPtr<InspectManager::Info> info, std::string name, zx::vmo vmo,
                             uint32_t protocol_id)
    : info_(std::move(info)), protocol_id_(protocol_id), name_(std::move(name)) {
  // Devices are sometimes passed bogus handles. Fun!
  if (vmo.is_valid()) {
    dev_vmo_.emplace(std::move(vmo));
  }
  device_node_ = info_->devices.CreateChild(name_);
  // Increment device count.
  info_->device_count.Add(1);
}

DeviceInspect::~DeviceInspect() {
  if (info_) {
    // Decrement device count.
    info_->device_count.Subtract(1);
  }
}

DeviceInspect DeviceInspect::CreateChild(std::string name, zx::vmo vmo, uint32_t protocol_id) {
  return DeviceInspect(info_, std::move(name), std::move(vmo), protocol_id);
}

zx::result<> DeviceInspect::Publish() {
  if (!dev_vmo_.has_value()) {
    return zx::ok();
  }

  zx::result link_name = info_->devfs.Publish(name_.c_str(), std::move(*dev_vmo_));
  dev_vmo_.reset();
  if (link_name.is_error()) {
    return link_name.take_error();
  }

  link_name_ = link_name.value();
  return zx::ok();
}

void DeviceInspect::SetStaticValues(const std::string& topological_path, uint32_t protocol_id,
                                    const std::string& type,
                                    const cpp20::span<const zx_device_prop_t>& properties,
                                    const std::string& driver_url) {
  protocol_id_ = protocol_id;
  device_node_.CreateString("topological_path", topological_path, &static_values_);
  device_node_.CreateUint("protocol_id", protocol_id, &static_values_);
  device_node_.CreateString("type", type, &static_values_);
  device_node_.CreateString("driver", driver_url, &static_values_);

  inspect::Node properties_array;

  // Add a node only if there are any `props`
  if (!properties.empty()) {
    properties_array = device_node_.CreateChild("properties");
  }

  for (uint32_t i = 0; i < properties.size(); ++i) {
    const zx_device_prop_t* p = &properties[i];
    const char* param_name = BindParamName(p->id);
    auto property = properties_array.CreateChild(std::to_string(i));
    property.CreateUint("value", p->value, &static_values_);
    if (param_name) {
      property.CreateString("id", param_name, &static_values_);
    } else {
      property.CreateString("id", std::to_string(p->id), &static_values_);
    }
    static_values_.emplace(std::move(property));
  }

  // Place the node into value list as props will not change in the lifetime of the device.
  if (!properties.empty()) {
    static_values_.emplace(std::move(properties_array));
  }
}

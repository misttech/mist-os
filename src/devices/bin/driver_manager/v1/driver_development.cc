// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/driver_development.h"

#include <lib/ddk/driver.h>

#include "src/devices/bin/driver_manager/v1/composite_device.h"
#include "src/devices/bin/driver_manager/v1/driver_host.h"

namespace fdd = fuchsia_driver_development;
namespace fdl = fuchsia_driver_legacy;

namespace {

const char* get_protocol_name(uint32_t protocol_id) {
  switch (protocol_id) {
#define DDK_PROTOCOL_DEF(tag, val, name, flags) \
  case val:                                     \
    return "ZX_PROTOCOL_" #tag;
#include <lib/ddk/protodefs.h>
    default:
      return "unknown";
  }
}

}  // namespace

zx::result<std::vector<fdd::wire::NodeInfo>> GetDeviceInfo(
    fidl::AnyArena& allocator, const std::vector<fbl::RefPtr<const Device>>& devices) {
  std::vector<fdd::wire::NodeInfo> device_info_vec;
  for (const auto& device : devices) {
    if (device->props().size() > fuchsia_driver_legacy::wire::kPropertiesMax) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }
    if (device->str_props().size() > fuchsia_driver_legacy::wire::kPropertiesMax) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }

    auto device_info = fdd::wire::NodeInfo::Builder(allocator);

    // id leaks internal pointers, but since this is a development only API, it shouldn't be
    // a big deal.
    device_info.id(reinterpret_cast<uint64_t>(device.get()));

    // TODO(fxbug.dev/80094): Handle multiple parents case.
    fidl::VectorView<uint64_t> parent_ids(allocator, 1);
    parent_ids[0] = reinterpret_cast<uint64_t>(device->parent().get());
    device_info.parent_ids(parent_ids);

    size_t child_count = 0;
    for (const Device* child __attribute__((unused)) : device->children()) {
      child_count++;
    }
    if (child_count > 0) {
      fidl::VectorView<uint64_t> child_ids(allocator, child_count);
      size_t i = 0;
      for (const Device* child : device->children()) {
        child_ids[i++] = reinterpret_cast<uint64_t>(child);
      }
      device_info.child_ids(child_ids);
    }

    if (device->host()) {
      device_info.driver_host_koid(device->host()->koid());
    }

    auto v1_info_builder = fuchsia_driver_development::wire::V1DeviceInfo::Builder(allocator);

    v1_info_builder.topological_path(device->MakeTopologicalPath());

    auto bound_driver_component_url = device->bound_driver_component_url();
    if (bound_driver_component_url.has_value()) {
      device_info.bound_driver_url(fidl::StringView(allocator, bound_driver_component_url.value()));
    }

    v1_info_builder.bound_driver_libname(fidl::StringView(allocator, device->parent_driver_url()));

    fidl::VectorView<fuchsia_driver_legacy::wire::DeviceProperty> props(allocator,
                                                                        device->props().size());
    for (size_t i = 0; i < device->props().size(); i++) {
      const auto& prop = device->props()[i];
      props[i] = fuchsia_driver_legacy::wire::DeviceProperty{
          .id = prop.id,
          .reserved = prop.reserved,
          .value = prop.value,
      };
    }

    fidl::VectorView<fuchsia_driver_legacy::wire::DeviceStrProperty> str_props(
        allocator, device->str_props().size());
    for (size_t i = 0; i < device->str_props().size(); i++) {
      const auto& str_prop = device->str_props()[i];
      if (str_prop.value.valueless_by_exception()) {
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto fidl_str_prop = fuchsia_driver_legacy::wire::DeviceStrProperty{
          .key = fidl::StringView(allocator, str_prop.key),
      };

      switch (str_prop.value.index()) {
        case StrPropValueType::Integer: {
          const auto prop_val = std::get<StrPropValueType::Integer>(str_prop.value);
          fidl_str_prop.value = fuchsia_driver_legacy::wire::PropertyValue::WithIntValue(prop_val);
          break;
        }
        case StrPropValueType::String: {
          const auto prop_val = std::get<StrPropValueType::String>(str_prop.value);
          fidl_str_prop.value = fuchsia_driver_legacy::wire::PropertyValue::WithStrValue(
              allocator, fidl::StringView(allocator, prop_val));
          break;
        }
        case StrPropValueType::Bool: {
          const auto prop_val = std::get<StrPropValueType::Bool>(str_prop.value);
          fidl_str_prop.value = fuchsia_driver_legacy::wire::PropertyValue::WithBoolValue(prop_val);
          break;
        }
        case StrPropValueType::Enum: {
          const auto prop_val = std::get<StrPropValueType::Enum>(str_prop.value);
          fidl_str_prop.value = fuchsia_driver_legacy::wire::PropertyValue::WithEnumValue(
              allocator, fidl::StringView(allocator, prop_val));
          break;
        }
      }

      str_props[i] = fidl_str_prop;
    }

    v1_info_builder.property_list(fuchsia_driver_legacy::wire::DevicePropertyList{
        .props = props,
        .str_props = str_props,
    });

    v1_info_builder.flags(fdl::DeviceFlags::TruncatingUnknown(device->flags));

    if (device->protocol_id() != 0 && device->protocol_id() != ZX_PROTOCOL_MISC) {
      v1_info_builder.protocol_id(device->protocol_id());
      const char* protocol_name = get_protocol_name(device->protocol_id());
      v1_info_builder.protocol_name(allocator, std::string_view(protocol_name));
    }

    auto versioned_info = fuchsia_driver_development::wire::VersionedNodeInfo::WithV1(
        allocator, v1_info_builder.Build());
    device_info.versioned_info(versioned_info);

    device_info_vec.push_back(device_info.Build());
  }
  return zx::ok(std::move(device_info_vec));
}

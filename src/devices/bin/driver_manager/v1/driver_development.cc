// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/driver_development.h"

#include <lib/ddk/driver.h>

#include "src/devices/bin/driver_manager/v1/composite_device.h"

namespace fdd = fuchsia_driver_development;
namespace fdm = fuchsia_device_manager;

namespace {

constexpr size_t kMaxEntries = 100;

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

zx::result<std::vector<fdd::wire::DeviceInfo>> GetDeviceInfo(
    fidl::AnyArena& allocator, const std::vector<fbl::RefPtr<const Device>>& devices) {
  std::vector<fdd::wire::DeviceInfo> device_info_vec;
  for (const auto& device : devices) {
    if (device->props().size() > fdm::wire::kPropertiesMax) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }
    if (device->str_props().size() > fdm::wire::kPropertiesMax) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }

    auto device_info = fdd::wire::DeviceInfo::Builder(allocator);

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

    device_info.topological_path(device->MakeTopologicalPath());

    device_info.bound_driver_libname(fidl::StringView(allocator, device->parent_driver_url()));

    fidl::VectorView<fdm::wire::DeviceProperty> props(allocator, device->props().size());
    for (size_t i = 0; i < device->props().size(); i++) {
      const auto& prop = device->props()[i];
      props[i] = fdm::wire::DeviceProperty{
          .id = prop.id,
          .reserved = prop.reserved,
          .value = prop.value,
      };
    }

    fidl::VectorView<fdm::wire::DeviceStrProperty> str_props(allocator, device->str_props().size());
    for (size_t i = 0; i < device->str_props().size(); i++) {
      const auto& str_prop = device->str_props()[i];
      if (str_prop.value.valueless_by_exception()) {
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto fidl_str_prop = fdm::wire::DeviceStrProperty{
          .key = fidl::StringView(allocator, str_prop.key),
      };

      switch (str_prop.value.index()) {
        case StrPropValueType::Integer: {
          const auto prop_val = std::get<StrPropValueType::Integer>(str_prop.value);
          fidl_str_prop.value = fdm::wire::PropertyValue::WithIntValue(prop_val);
          break;
        }
        case StrPropValueType::String: {
          const auto prop_val = std::get<StrPropValueType::String>(str_prop.value);
          fidl_str_prop.value = fdm::wire::PropertyValue::WithStrValue(
              allocator, fidl::StringView(allocator, prop_val));
          break;
        }
        case StrPropValueType::Bool: {
          const auto prop_val = std::get<StrPropValueType::Bool>(str_prop.value);
          fidl_str_prop.value = fdm::wire::PropertyValue::WithBoolValue(prop_val);
          break;
        }
        case StrPropValueType::Enum: {
          const auto prop_val = std::get<StrPropValueType::Enum>(str_prop.value);
          fidl_str_prop.value = fdm::wire::PropertyValue::WithEnumValue(
              allocator, fidl::StringView(allocator, prop_val));
          break;
        }
      }

      str_props[i] = fidl_str_prop;
    }

    device_info.property_list(fdm::wire::DevicePropertyList{
        .props = props,
        .str_props = str_props,
    });

    device_info.flags(fdd::wire::DeviceFlags::TruncatingUnknown(device->flags));

    if (device->protocol_id() != 0 && device->protocol_id() != ZX_PROTOCOL_MISC) {
      device_info.protocol_id(device->protocol_id());
      const char* protocol_name = get_protocol_name(device->protocol_id());
      device_info.protocol_name(allocator, std::string_view(protocol_name));
    }

    device_info_vec.push_back(device_info.Build());
  }
  return zx::ok(std::move(device_info_vec));
}

void DeviceInfoIterator::GetNext(GetNextCompleter::Sync& completer) {
  if (offset_ >= list_.size()) {
    completer.Reply(fidl::VectorView<fdd::wire::DeviceInfo>{});
    return;
  }

  auto result = cpp20::span(&list_[offset_], std::min(kMaxEntries, list_.size() - offset_));
  offset_ += result.size();

  completer.Reply(
      fidl::VectorView<fdd::wire::DeviceInfo>::FromExternal(result.data(), result.size()));
}

void CompositeInfoIterator::GetNext(GetNextCompleter::Sync& completer) {
  if (offset_ >= list_.size()) {
    completer.Reply(fidl::VectorView<fdd::wire::CompositeInfo>{});
    return;
  }

  auto result = cpp20::span(&list_[offset_], std::min(kMaxEntries, list_.size() - offset_));
  offset_ += result.size();
  completer.Reply(
      fidl::VectorView<fdd::wire::CompositeInfo>::FromExternal(result.data(), result.size()));
}

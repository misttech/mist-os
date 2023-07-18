// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "composite_device.h"

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <zircon/status.h>

#include "src/devices/bin/driver_manager/v1/bind_driver_manager.h"
#include "src/devices/bin/driver_manager/v1/coordinator.h"
#include "src/devices/bin/driver_manager/v1/driver_host.h"
#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;
namespace fdm = fuchsia_device_manager;

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace {

fbl::Array<StrProperty> ConvertStringProperties(
    fidl::VectorView<fdm::wire::DeviceStrProperty> str_props) {
  fbl::Array<StrProperty> str_properties(new StrProperty[str_props.count()], str_props.count());
  for (size_t i = 0; i < str_props.count(); i++) {
    str_properties[i].key = str_props[i].key.get();
    if (str_props[i].value.is_int_value()) {
      str_properties[i].value.emplace<StrPropValueType::Integer>(str_props[i].value.int_value());
    } else if (str_props[i].value.is_str_value()) {
      str_properties[i].value.emplace<StrPropValueType::String>(
          std::string(str_props[i].value.str_value().get()));
    } else if (str_props[i].value.is_bool_value()) {
      str_properties[i].value.emplace<StrPropValueType::Bool>(str_props[i].value.bool_value());
    } else if (str_props[i].value.is_enum_value()) {
      str_properties[i].value.emplace<StrPropValueType::Enum>(
          std::string(str_props[i].value.enum_value().get()));
    }
  }

  return str_properties;
}

fidl::VectorView<fdf::wire::NodeProperty> ConvertToNodeProperties(
    fidl::AnyArena& arena, const fbl::Array<const zx_device_prop_t>& props,
    const fbl::Array<const StrProperty>& str_props) {
  size_t size = props.size() + str_props.size();
  size_t index = 0;

  fidl::VectorView<fdf::wire::NodeProperty> fidl_props(arena, size);
  for (size_t i = 0; i < props.size(); i++) {
    fidl_props[index++] = fdf::MakeProperty(arena, props[i].id, props[i].value);
  }

  for (size_t i = 0; i < str_props.size(); i++) {
    switch (str_props[i].value.index()) {
      case StrPropValueType::Integer: {
        fidl_props[index++] = fdf::MakeProperty(
            arena, str_props[i].key, std::get<StrPropValueType::Integer>(str_props[i].value));
        break;
      }
      case StrPropValueType::String: {
        fidl_props[index++] = fdf::MakeProperty(
            arena, str_props[i].key, std::get<StrPropValueType::String>(str_props[i].value));
        break;
      }
      case StrPropValueType::Bool: {
        fidl_props[index++] = fdf::MakeProperty(
            arena, str_props[i].key, std::get<StrPropValueType::Bool>(str_props[i].value));
        break;
      }
      case StrPropValueType::Enum: {
        fidl_props[index++] = fdf::MakeProperty(
            arena, str_props[i].key, std::get<StrPropValueType::Enum>(str_props[i].value));
        break;
      }
    }
  }

  return fidl_props;
}

}  // namespace

// CompositeDevice methods

CompositeDevice::CompositeDevice(fbl::String name, fbl::Array<const zx_device_prop_t> properties,
                                 fbl::Array<const StrProperty> str_properties,
                                 uint32_t fragments_count, uint32_t primary_fragment_index,
                                 std::optional<bool> legacy_colocate_flag,
                                 fbl::Array<std::unique_ptr<Metadata>> metadata,
                                 Coordinator& coordinator, bool from_composite_node_spec)
    : name_(std::move(name)),
      properties_(std::move(properties)),
      str_properties_(std::move(str_properties)),
      fragments_count_(fragments_count),
      primary_fragment_index_(primary_fragment_index),
      legacy_colocate_flag_(legacy_colocate_flag),
      metadata_(std::move(metadata)),
      from_composite_node_spec_(from_composite_node_spec),
      coordinator_(coordinator) {}

CompositeDevice::~CompositeDevice() = default;

zx_status_t CompositeDevice::Create(std::string_view name,
                                    fdm::wire::CompositeDeviceDescriptor comp_desc,
                                    Coordinator& coordinator,
                                    std::unique_ptr<CompositeDevice>* out) {
  fbl::String name_obj(name);
  fbl::Array<zx_device_prop_t> properties(new zx_device_prop_t[comp_desc.props.count() + 1],
                                          comp_desc.props.count() + 1);
  memcpy(properties.data(), comp_desc.props.data(),
         comp_desc.props.count() * sizeof(comp_desc.props.data()[0]));

  // Set a property unique to composite devices.
  properties[comp_desc.props.count()].id = BIND_COMPOSITE;
  properties[comp_desc.props.count()].value = 1;

  fbl::Array<std::unique_ptr<Metadata>> metadata(
      new std::unique_ptr<Metadata>[comp_desc.metadata.count()], comp_desc.metadata.count());

  fbl::Array<StrProperty> str_properties = ConvertStringProperties(comp_desc.str_props);

  for (size_t i = 0; i < comp_desc.metadata.count(); i++) {
    std::unique_ptr<Metadata> md;
    zx_status_t status = Metadata::Create(comp_desc.metadata[i].data.count(), &md);
    if (status != ZX_OK) {
      return status;
    }

    md->type = comp_desc.metadata[i].key;
    md->length = static_cast<uint32_t>(comp_desc.metadata[i].data.count());
    memcpy(md->Data(), comp_desc.metadata[i].data.data(), md->length);
    metadata[i] = std::move(md);
  }

  auto dev = std::make_unique<CompositeDevice>(
      std::move(name), std::move(properties), std::move(str_properties),
      comp_desc.fragments.count(), comp_desc.primary_fragment_index, comp_desc.spawn_colocated,
      std::move(metadata), coordinator, false);
  for (uint32_t i = 0; i < comp_desc.fragments.count(); ++i) {
    const auto& fidl_fragment = comp_desc.fragments[i];
    size_t parts_count = fidl_fragment.parts.count();
    if (parts_count != 1) {
      LOGF(ERROR, "Composite fragments with multiple parts are deprecated. %s has %zd parts.",
           name.data(), parts_count);
      return ZX_ERR_INVALID_ARGS;
    }

    const auto& fidl_part = fidl_fragment.parts[0];
    size_t program_count = fidl_part.match_program.count();
    fbl::Array<zx_bind_inst_t> bind_rules(new zx_bind_inst_t[program_count], program_count);
    for (size_t j = 0; j < program_count; ++j) {
      bind_rules[j] = zx_bind_inst_t{
          .op = fidl_part.match_program[j].op,
          .arg = fidl_part.match_program[j].arg,
      };
    }
    std::string name(fidl_fragment.name.data(), fidl_fragment.name.size());
    auto fragment =
        std::make_unique<CompositeDeviceFragment>(dev.get(), name, i, std::move(bind_rules));
    dev->fragments_.push_back(std::move(fragment));
  }
  *out = std::move(dev);
  return ZX_OK;
}

std::unique_ptr<CompositeDevice> CompositeDevice::CreateFromSpec(
    CompositeNodeSpecInfo composite_info, fbl::Array<std::unique_ptr<Metadata>> metadata,
    Coordinator& coordinator) {
  uint32_t num_parents = static_cast<uint32_t>(composite_info.parent_names.size());

  auto dev = std::make_unique<CompositeDevice>(
      fbl::String(composite_info.composite_name), fbl::Array<const zx_device_prop_t>(),
      fbl::Array<const StrProperty>(), num_parents, composite_info.primary_index, std::nullopt,
      std::move(metadata), coordinator, true);

  for (uint32_t i = 0; i < num_parents; ++i) {
    std::string name = composite_info.parent_names[i];
    auto fragment = std::make_unique<CompositeDeviceFragment>(dev.get(), std::string(name), i,
                                                              fbl::Array<const zx_bind_inst_t>());
    dev->fragments_.push_back(std::move(fragment));
  }
  dev->driver_.emplace(composite_info.driver);
  return dev;
}

bool CompositeDevice::IsFragmentMatch(const fbl::RefPtr<Device>& dev, size_t* index_out) const {
  if (from_composite_node_spec_) {
    return false;
  }

  // Check bound fragments for ambiguous binds.
  for (auto& fragment : fragments_) {
    if (!fragment.IsBound()) {
      continue;
    }

    if (fragment.TryMatch(dev)) {
      LOGF(ERROR, "Ambiguous bind for composite device %p '%s': device 1 '%s', device 2 '%s'", this,
           name_.data(), fragment.bound_device()->name().data(), dev->name().data());
      return false;
    }
  }

  // Check unbound fragments for matches.
  for (auto& fragment : fragments_) {
    if (fragment.IsBound()) {
      continue;
    }

    if (fragment.TryMatch(dev)) {
      VLOGF(1, "Found a match for composite device %p '%s': device '%s'", this, name_.data(),
            dev->name().data());
      *index_out = fragment.index();
      return true;
    }
  }

  VLOGF(1, "No match for composite device %p '%s': device '%s'", this, name_.data(),
        dev->name().data());
  return false;
}

zx::result<> CompositeDevice::MatchDriverToComposite() {
  ZX_ASSERT(!driver_.has_value());
  ZX_ASSERT(!from_composite_node_spec_);

  auto match_result = coordinator_.bind_driver_manager().MatchCompositeDevice(
      *this, DriverLoader::MatchDeviceConfig{});
  if (match_result.is_error()) {
    if (match_result.status_value() != ZX_ERR_NOT_FOUND) {
      LOGF(ERROR, "Failed to match driver to composite: %s",
           zx_status_get_string(match_result.status_value()));
    }
    return match_result.take_error();
  }

  driver_.emplace(match_result.value());
  return zx::ok();
}

zx_status_t CompositeDevice::TryMatchBindFragments(const fbl::RefPtr<Device>& dev) {
  if (from_composite_node_spec_) {
    return ZX_OK;
  }

  if (!driver_.has_value()) {
    auto result = MatchDriverToComposite();
    if (result.is_error()) {
      return result.status_value() == ZX_ERR_NOT_FOUND ? ZX_OK : result.status_value();
    }
  }

  size_t index;
  if (!IsFragmentMatch(dev, &index)) {
    return ZX_OK;
  }

  LOGF(DEBUG, "Device '%s' matched fragment %zu of composite '%s'", dev->name().data(), index,
       name().data());
  auto status = BindFragment(index, dev);
  if (status != ZX_OK) {
    LOGF(ERROR, "Device '%s' failed to bind fragment %zu of composite '%s': %s", dev->name().data(),
         index, name().data(), zx_status_get_string(status));
  }
  return status;
}

zx_status_t CompositeDevice::BindFragment(size_t index, const fbl::RefPtr<Device>& dev) {
  // Find the fragment we're binding
  CompositeDeviceFragment* fragment = nullptr;
  for (auto& f : fragments_) {
    if (f.index() == index) {
      fragment = &f;
      break;
    }
  }

  if (!fragment || fragment->IsBound()) {
    LOGF(ERROR, "Attempted to bind bound fragment %zu in composite device %p", index, name_.data());
    return ZX_OK;
  }

  zx_status_t status = fragment->Bind(dev);
  if (status != ZX_OK) {
    return status;
  }

  // If the device uses the fragment driver, then `CompositeDeviceFragment::Bind` will bind the
  // fragment driver to the device, and `DeviceManager` will call `TryAssemble` once the fragment
  // device is added. If the device does not use the fragment driver, then this will not happen, so
  // we have to call `TryAssemble` explicitly here.
  if (!fragment->uses_fragment_driver()) {
    status = TryAssemble();
    if (status != ZX_OK && status != ZX_ERR_SHOULD_WAIT) {
      LOGF(ERROR, "Failed to assemble composite device: %s", zx_status_get_string(status));
      return status;
    }
  }

  return ZX_OK;
}

zx::result<fbl::RefPtr<DriverHost>> CompositeDevice::GetDriverHost() {
  ZX_ASSERT(driver_.has_value());

  // We already have a driver host so return it.
  if (driver_host_) {
    return zx::ok(driver_host_);
  }

  // The composite should be colocated if both its colocate flags from device_add_composite()
  // and the driver CML are set to true. If the CompositeDevice isn't created from
  // device_add_composite(), we will only use the colocate flag from the driver CML.
  // TODO(fxb/121385): Remove the legacy colocate flag and the warning log once the migration
  // is completed.
  bool should_colocate = driver_->colocate;
  if (legacy_colocate_flag_.has_value()) {
    if (!legacy_colocate_flag_.value() && driver_->colocate) {
      LOGF(WARNING,
           "Inconsistent colocation flags in driver: %s. The legacy flag is %s "
           "whereas the Driver CML colocate value is %s. This is only an issue if you "
           "want the driver to be co-located. See fxb/121385 for more details.",
           driver_.value().component_url.c_str(), legacy_colocate_flag_.value() ? "true" : "false",
           driver_.value().colocate ? "true" : "false");
    }

    should_colocate = driver_->colocate && legacy_colocate_flag_.value();
  }

  // If the composite should be spawn colocated, set the |driver_host_| to be the same as the
  // primary fragment and return it.
  if (should_colocate) {
    if (const CompositeDeviceFragment* fragment = GetPrimaryFragment(); fragment) {
      driver_host_ = fragment->bound_device()->host();
    }
    return zx::ok(driver_host_);
  }

  // Create a new driver host and return it.
  auto status = coordinator_.NewDriverHost("driver_host", &driver_host_);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(driver_host_);
}

zx_status_t CompositeDevice::TryAssemble() {
  ZX_ASSERT(!device_);

  // If unavailable, search for a driver that binds to the composite.
  if (!driver_.has_value()) {
    auto result = MatchDriverToComposite();
    if (result.is_error()) {
      return result.status_value() == ZX_ERR_NOT_FOUND ? ZX_OK : ZX_ERR_SHOULD_WAIT;
    }
  }

  for (auto& fragment : fragments_) {
    if (!fragment.IsReady()) {
      return ZX_ERR_SHOULD_WAIT;
    }
  }

  zx::result driver_host = GetDriverHost();

  fidl::Arena arena;
  fidl::VectorView<fdm::wire::Fragment> fragments(arena, fragments_.size_slow());

  // Create all of the proxies for the fragment devices, in the same process
  for (auto& fragment : fragments_) {
    zx_status_t status = fragment.CreateProxy(driver_host.value());
    if (status != ZX_OK) {
      return status;
    }
    // Stash the local ID after the proxy has been created
    fragments[fragment.index()].name = fidl::StringView::FromExternal(fragment.name());
    fragments[fragment.index()].id = fragment.proxy_device()->local_id();
  }

  auto coordinator_endpoints = fidl::CreateEndpoints<fdm::Coordinator>();
  if (coordinator_endpoints.is_error()) {
    return coordinator_endpoints.error_value();
  }

  auto device_controller_endpoints = fidl::CreateEndpoints<fdm::DeviceController>();
  if (device_controller_endpoints.is_error()) {
    return device_controller_endpoints.error_value();
  }

  fbl::RefPtr<Device> new_device;
  auto status = Device::CreateComposite(
      &coordinator_, driver_host.value(), *this, std::move(coordinator_endpoints->server),
      std::move(device_controller_endpoints->client), &new_device);
  if (status != ZX_OK) {
    return status;
  }
  coordinator_.device_manager()->AddToDevices(new_device);

  // Create the composite device in the driver_host
  fdm::wire::CompositeDevice composite{fragments, fidl::StringView::FromExternal(name())};
  auto type = fdm::wire::DeviceType::WithComposite(arena, composite);

  driver_host->controller()
      ->CreateDevice(std::move(coordinator_endpoints->client),
                     std::move(device_controller_endpoints->server), std::move(type),
                     new_device->local_id())
      .ThenExactlyOnce(
          [](fidl::WireUnownedResult<fdm::DriverHostController::CreateDevice>& result) {
            if (!result.ok()) {
              LOGF(ERROR, "Failed to create composite device: %s",
                   result.error().FormatDescription().c_str());
              return;
            }
            if (result.value().status != ZX_OK) {
              LOGF(ERROR, "Failed to create composite device: %s",
                   zx_status_get_string(result.value().status));
            }
          });

  device_ = std::move(new_device);

  // Add metadata
  for (size_t i = 0; i < metadata_.size(); i++) {
    // Making a copy of metadata, instead of transfering ownership, so that
    // metadata can be added again if device is recreated
    status = coordinator_.AddMetadata(device_, metadata_[i]->type, metadata_[i]->Data(),
                                      metadata_[i]->length);
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to add metadata to device %p '%s': %s", device_.get(),
           device_->name().data(), zx_status_get_string(status));
      return status;
    }
  }

  // Bind |driver_| to |device_|.
  status = coordinator_.AttemptBind(driver_.value(), device_);
  if (status != ZX_OK) {
    LOGF(ERROR, "%s: Failed to bind driver '%s' to device '%s': %s", __func__,
         driver_.value().component_url.c_str(), device_->name().data(),
         zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

void CompositeDevice::SetMatchedDriver(MatchedDriverInfo driver) {
  ZX_ASSERT(!driver_.has_value());
  driver_.emplace(driver);
}

void CompositeDevice::UnbindFragment(CompositeDeviceFragment* fragment) {
  // If the composite was fully instantiated, disassociate from it.  It will be
  // reinstantiated when this fragment is re-bound.
  if (device_) {
    Remove();
  }
  ZX_ASSERT(!device_);
  ZX_ASSERT(fragment->composite() == this);
}

void CompositeDevice::Remove() {
  if (device_) {
    device_->disassociate_from_composite();
    device_ = nullptr;
  }
  driver_host_ = nullptr;
}

fdd::wire::CompositeInfo CompositeDevice::GetCompositeInfo(fidl::AnyArena& arena) const {
  auto composite_info = fdd::wire::CompositeInfo::Builder(arena)
                            .name(fidl::StringView(arena, name_.c_str()))
                            .primary_index(primary_fragment_index_);

  if (driver_.has_value()) {
    composite_info.driver(driver_->component_url);
  }

  if (from_composite_node_spec_) {
    composite_info.node_info(
        fdd::wire::CompositeNodeInfo::WithParents(arena, GetParentInfo(arena)));
  } else {
    composite_info.node_info(
        fdd::wire::CompositeNodeInfo::WithLegacy(arena, GetLegacyCompositeInfo(arena)));
  }

  if (device_) {
    composite_info.topological_path(device_->MakeTopologicalPath());
  }

  return composite_info.Build();
}

fdd::wire::LegacyCompositeNodeInfo CompositeDevice::GetLegacyCompositeInfo(
    fidl::AnyArena& arena) const {
  auto legacy_info = fdd::wire::LegacyCompositeNodeInfo::Builder(arena).properties(
      ConvertToNodeProperties(arena, properties_, str_properties_));

  fidl::VectorView<fdd::wire::LegacyCompositeFragmentInfo> fragments(arena, fragments_count_);
  uint32_t index = 0;
  for (auto& fragment : fragments_) {
    fragments[index] = fragment.GetCompositeFragmentInfo(arena);
    index++;
  }
  legacy_info.fragments(fragments);
  return legacy_info.Build();
}

fidl::VectorView<fdd::wire::CompositeParentNodeInfo> CompositeDevice::GetParentInfo(
    fidl::AnyArena& arena) const {
  fidl::VectorView<fdd::wire::CompositeParentNodeInfo> parents(arena, fragments_count_);
  uint32_t index = 0;
  for (auto& fragment : fragments_) {
    auto parent = fdd::wire::CompositeParentNodeInfo::Builder(arena).name(
        fidl::StringView(arena, std::string(fragment.name())));

    if (fragment.bound_device()) {
      parent.device(arena, fragment.bound_device()->MakeTopologicalPath());
    }

    parents[index] = parent.Build();
    index++;
  }
  return parents;
}

CompositeDeviceFragment* CompositeDevice::GetPrimaryFragment() {
  for (auto& fragment : fragments_) {
    if (fragment.index() == primary_fragment_index_) {
      return &fragment;
    }
  }
  return nullptr;
}

const CompositeDeviceFragment* CompositeDevice::GetPrimaryFragment() const {
  for (auto& fragment : fragments_) {
    if (fragment.index() == primary_fragment_index_) {
      return &fragment;
    }
  }
  return nullptr;
}

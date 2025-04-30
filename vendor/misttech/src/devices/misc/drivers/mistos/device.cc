// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/device.h"

#include <lib/mistos/devmgr/coordinator.h>
#include <lib/mistos/devmgr/init.h>
#include <trace.h>

#include <algorithm>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <ddktl/device.h>

#include "misc/drivers/mistos/driver.h"
#include "misc/drivers/mistos/symbols.h"

#define LOCAL_TRACE 0

namespace {

struct ProtocolInfo {
  std::string_view name;
  uint32_t id;
  uint32_t flags;
};

template <typename T>
bool HasOp(const zx_protocol_device_t* ops, T member) {
  return ops != nullptr && ops->*member != nullptr;
}

}  // namespace

namespace mistos {

fbl::Vector<zx_device_str_prop> CreateProperties(device_add_args_t* zx_args) {
  fbl::AllocChecker ac;
  fbl::Vector<zx_device_str_prop> properties;
  properties.reserve(zx_args->str_prop_count, &ac);
  ZX_ASSERT(ac.check());
  bool has_protocol = false;
  for (auto [key, value] : cpp20::span(zx_args->str_props, zx_args->str_prop_count)) {
    if (key == bind_fuchsia::PROTOCOL) {
      has_protocol = true;
    }
    switch (value.data_type) {
      case ZX_DEVICE_PROPERTY_VALUE_BOOL:
        properties.push_back(ddk::MakeStrProperty(key, value.data.bool_val), &ac);
        ZX_ASSERT(ac.check());
        break;
      case ZX_DEVICE_PROPERTY_VALUE_STRING:
        properties.push_back(ddk::MakeStrProperty(key, value.data.str_val), &ac);
        ZX_ASSERT(ac.check());
        break;
      case ZX_DEVICE_PROPERTY_VALUE_INT:
        properties.push_back(ddk::MakeStrProperty(key, value.data.int_val), &ac);
        ZX_ASSERT(ac.check());
        break;
      case ZX_DEVICE_PROPERTY_VALUE_ENUM:
        properties.push_back(ddk::MakeStrProperty(key, value.data.enum_val), &ac);
        ZX_ASSERT(ac.check());
        break;
      default:
        LTRACEF("Unsupported property type, key: %s\n", key);
        break;
    }
  }

  // Some DFv1 devices expect to be able to set their own protocol, without specifying proto_id.
  // If we see a BIND_PROTOCOL property, don't add our own.
  if (!has_protocol) {
    // If we do not have a protocol id, set it to MISC to match DFv1 behavior.
    uint32_t proto_id = zx_args->proto_id == 0 ? ZX_PROTOCOL_MISC : zx_args->proto_id;
    properties.push_back(ddk::MakeStrProperty(bind_fuchsia::PROTOCOL, proto_id), &ac);
    ZX_ASSERT(ac.check());
  }
  return properties;
}

Device::Device(device_t device, const zx_protocol_device_t* ops, Driver* driver,
               std::optional<Device*> parent)
    : name_(device.name), driver_(driver), compat_symbol_(device), ops_(ops), parent_(parent) {}

Device::~Device() {
  if (!children_.is_empty()) {
    LTRACEF("%s: Destructing device, but still had %zu children\n", Name(), children_.size());
    // Ensure we do not get use-after-free from calling child_pre_release
    // on a destructed parent device.
    // children_.clear();
  }

  /*if (ShouldCallRelease()) {
    // Call the parent's pre-release.
    if (HasOp((*parent_)->ops_, &zx_protocol_device_t::child_pre_release)) {
      (*parent_)->ops_->child_pre_release((*parent_)->compat_symbol_.context,
                                          compat_symbol_.context);
    }

    if (!release_after_dispatcher_shutdown_ && HasOp(ops_, &zx_protocol_device_t::release)) {
      ops_->release(compat_symbol_.context);
    }
  }

  for (auto& completer : remove_completers_) {
    completer.complete_ok();
  }*/
}

zx_device_t* Device::ZxDevice() { return static_cast<zx_device_t*>(this); }

const char* Device::Name() const { return name_.c_str(); }

bool Device::HasChildren() const { return !children_.is_empty(); }

zx_status_t Device::Add(device_add_args_t* zx_args, zx_device_t** out) {
  if (HasChildNamed(zx_args->name)) {
    return ZX_ERR_BAD_STATE;
  }

  // if (stop_triggered()) {
  //   return ZX_ERR_BAD_STATE;
  // }

  device_t compat_device = {
      .name = zx_args->name,
      .context = zx_args->ctx,
  };

  fbl::AllocChecker ac;
  auto device = ktl::make_unique<Device>(&ac, compat_device, zx_args->ops, driver_, this);
  // Update the compat symbol name pointer with a pointer the device owns.
  device->compat_symbol_.name = device->name_.c_str();

  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (driver()) {
    device->device_id_ = driver()->GetNextDeviceId();
  }

  DeviceServer::BanjoConfig banjo_config{.default_proto_id = zx_args->proto_id};

  // Set the callback specifically for the base proto_id if there is one.
  if (zx_args->proto_ops != nullptr && zx_args->proto_id != 0) {
    banjo_config.callbacks[zx_args->proto_id] = [ops = zx_args->proto_ops, ctx = zx_args->ctx]() {
      return DeviceServer::GenericProtocol{.ops = const_cast<void*>(ops), .ctx = ctx};
    };
  }

  // Set a generic callback for other proto_ids.
  banjo_config.generic_callback =
      [dev = device.get()](uint32_t proto_id) -> zx::result<DeviceServer::GenericProtocol> {
    DeviceServer::GenericProtocol protocol;
    if (HasOp(dev->ops_, &zx_protocol_device_t::get_protocol)) {
      zx_status_t status =
          dev->ops_->get_protocol(dev->compat_symbol_.context, proto_id, &protocol);
      if (status != ZX_OK) {
        return zx::error(status);
      }

      return zx::ok(protocol);
    }

    return zx::error(ZX_ERR_PROTOCOL_NOT_SUPPORTED);
  };

  device->device_server_.Initialize(std::move(banjo_config));

#if 0
  // Add the metadata from add_args:
  for (size_t i = 0; i < zx_args->metadata_count; i++) {
    auto status =
        device->AddMetadata(zx_args->metadata_list[i].type, zx_args->metadata_list[i].data,
                            zx_args->metadata_list[i].length);
    if (status != ZX_OK) {
      return status;
    }
  }
#endif

  device->properties_ = CreateProperties(zx_args);
  device->device_flags_ = zx_args->flags;

  if (out) {
    *out = device->ZxDevice();
  }

  if (HasOp(device->ops_, &zx_protocol_device_t::init)) {
    // We have to schedule the init task so that it is run in the dispatcher context,
    // as we are currently in the device context from device_add_from_driver().
    // (We are not allowed to re-enter the device context).
    /*device->executor_.schedule_task(fpromise::make_ok_promise().and_then(
        [device]() mutable { device->ops_->init(device->compat_symbol_.context); }));*/
    device->ops_->init(device->compat_symbol_.context);
  } else {
    device->InitReply(ZX_OK);
  }

  children_.push_back(ktl::move(device), &ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  return ZX_OK;
}

zx_status_t Device::ExportAfterInit() {
  LTRACE_EXIT_OBJ;
  if ((device_flags_ & DEVICE_ADD_NON_BINDABLE) != 0) {
    return ZX_OK;
  }

  fbl::AllocChecker ac;
  fbl::Vector<mistos::Symbol> symbols;
  symbols.push_back(mistos::Symbol{kDeviceSymbol, reinterpret_cast<uint64_t>(&compat_symbol_)},
                    &ac);
  ZX_ASSERT(ac.check());
  symbols.push_back(mistos::Symbol{kOps, reinterpret_cast<uint64_t>(ops_)}, &ac);
  ZX_ASSERT(ac.check());

  // Export parent as "default" symbol.
  symbols.push_back(mistos::Symbol{"default", reinterpret_cast<uint64_t>(this)}, &ac);
  ZX_ASSERT(ac.check());

  GlobalCoordinator().HandleNewDevice(std::move(mistos::DriverStartArgs(std::move(symbols))), this);
  return ZX_OK;
}

bool Device::HasChildNamed(std::string_view name) const {
  return std::ranges::any_of(children_,
                             [name](const auto& child) { return name == child->Name(); });
}

zx_status_t Device::GetProtocol(uint32_t proto_id, void* out) const {
  if (HasOp(ops_, &zx_protocol_device_t::get_protocol)) {
    return ops_->get_protocol(compat_symbol_.context, proto_id, out);
  }

  if (!device_server_.has_banjo_config()) {
    if (driver_ == nullptr) {
      LTRACEF("Driver is null\n");
      return ZX_ERR_BAD_STATE;
    }

    return driver_->GetProtocol(proto_id, out);
  }

  DeviceServer::GenericProtocol device_server_out;
  zx_status_t status = device_server_.GetProtocol(proto_id, &device_server_out);
  if (status != ZX_OK) {
    return status;
  }

  if (!out) {
    return ZX_OK;
  }

  struct GenericProtocol {
    const void* ops;
    void* ctx;
  };

  auto proto = static_cast<GenericProtocol*>(out);
  proto->ctx = device_server_out.ctx;
  proto->ops = device_server_out.ops;
  return ZX_OK;
}

zx_status_t Device::GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out) {
  if (driver() == nullptr) {
    LTRACEF("Driver is null\n");
    return ZX_ERR_BAD_STATE;
  }

  return driver()->GetFragmentProtocol(fragment, proto_id, out);
}

void Device::InitReply(zx_status_t status) {
#if 0
fpromise::promise<void, zx_status_t> promise =
      fpromise::make_result_promise<void, zx_status_t>(fpromise::ok());
  // If we have a parent, we want to only finish our init after they finish their init.
  if (parent_.has_value()) {
    promise = parent_.value()->WaitForInitToComplete();
  }
#endif

  if (parent_.has_value() && driver()) {
    if (status == ZX_OK) {
      status = ExportAfterInit();
      if (status != ZX_OK) {
        LTRACEF("Device %s failed to create: %s\n", Name(), zx_status_get_string(status));
      }
    }

    // We need to complete start after the first device the driver added completes it's init
    // hook.
    constexpr uint32_t kFirstDeviceId = 1;
    if (device_id_ == kFirstDeviceId) {
      if (status == ZX_OK) {
        driver()->CompleteStart(zx::ok());
      } else {
        driver()->CompleteStart(zx::error(status));
      }
    }
  }

  if (status != ZX_OK) {
    // Remove();
  }
}

}  // namespace mistos

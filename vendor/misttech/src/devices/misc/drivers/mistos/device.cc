// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/device.h"

#include <trace.h>

#include <algorithm>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

#include "misc/drivers/mistos/driver.h"
#include "misc/drivers/mistos/symbols.h"

#define LOCAL_TRACE 0

namespace {

struct ProtocolInfo {
  std::string_view name;
  uint32_t id;
  uint32_t flags;
};

// static constexpr ProtocolInfo kProtocolInfos[] = {
// #define DDK_PROTOCOL_DEF(tag, val, name, flags) {name, val, flags},
// #include <lib/ddk/protodefs.h>
// };

template <typename T>
bool HasOp(const zx_protocol_device_t* ops, T member) {
  return ops != nullptr && ops->*member != nullptr;
}

}  // namespace

namespace mistos {

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

const char* Device::Name() const { return name_.data(); }

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
  device->compat_symbol_.name = device->name_.data();

  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  if (driver()) {
    device->device_id_ = driver()->GetNextDeviceId();
  }

#if 0
  if (HasOp(dev->ops_, &zx_protocol_device_t::get_protocol)) {
    zx_status_t status = dev->ops_->get_protocol(dev->compat_symbol_.context, proto_id, &protocol);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    return zx::ok(protocol);
  }

  return zx::error(ZX_ERR_PROTOCOL_NOT_SUPPORTED);
#endif

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

  if (out) {
    *out = device->ZxDevice();
  }

  children_.push_back(ktl::move(device), &ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

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

  return driver_->GetProtocol(proto_id, out);
}

zx_status_t Device::GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out) {
  if (driver() == nullptr) {
    LTRACEF("Driver is null\n");
    return ZX_ERR_BAD_STATE;
  }

  return driver()->GetFragmentProtocol(fragment, proto_id, out);
}

void Device::InitReply(zx_status_t status) { driver()->CompleteStart(zx::ok()); }

}  // namespace mistos

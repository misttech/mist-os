// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v1/composite_device_fragment.h"

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <zircon/status.h>

#include "src/devices/bin/driver_manager/binding.h"
#include "src/devices/bin/driver_manager/v1/coordinator.h"
#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;
namespace fdm = fuchsia_device_manager;

CompositeDeviceFragment::CompositeDeviceFragment(CompositeDevice* composite, std::string name,
                                                 uint32_t index,
                                                 fbl::Array<const zx_bind_inst_t> bind_rules)
    : composite_(composite), name_(name), index_(index), bind_rules_(std::move(bind_rules)) {}

CompositeDeviceFragment::~CompositeDeviceFragment() = default;

fdd::wire::LegacyCompositeFragmentInfo CompositeDeviceFragment::GetCompositeFragmentInfo(
    fidl::AnyArena& arena) const {
  auto fragment_info = fdd::wire::LegacyCompositeFragmentInfo::Builder(arena).name(
      fidl::StringView(arena, name_.c_str()));

  if (bound_device_) {
    fragment_info.device(bound_device_->MakeTopologicalPath());
  }

  fidl::VectorView<fdm::wire::BindInstruction> bind_rules(arena, bind_rules_.size());
  for (size_t i = 0; i < bind_rules_.size(); i++) {
    bind_rules[i] = fdm::wire::BindInstruction{
        .op = bind_rules_[i].op,
        .arg = bind_rules_[i].arg,
        .debug = bind_rules_[i].debug,
    };
  }
  fragment_info.bind_rules(bind_rules);
  return fragment_info.Build();
}

bool CompositeDeviceFragment::TryMatch(const fbl::RefPtr<Device>& dev) const {
  internal::BindProgramContext ctx;
  ctx.props = &dev->props();
  ctx.protocol_id = dev->protocol_id();
  ctx.binding = bind_rules_.data();
  ctx.binding_size = bind_rules_.size() * sizeof(bind_rules_[0]);
  ctx.name = "composite_binder";
  ctx.autobind = true;
  return internal::EvaluateBindProgram(&ctx);
}

zx_status_t CompositeDeviceFragment::Bind(const fbl::RefPtr<Device>& dev) {
  ZX_ASSERT(!bound_device_);

  uses_fragment_driver_ = true;
  zx_status_t status = dev->coordinator->AttemptBind(fdf::kFragmentDriverInfo, dev);
  if (status != ZX_OK) {
    return status;
  }
  bound_device_ = dev;
  dev->push_fragment(this);

  return ZX_OK;
}

bool CompositeDeviceFragment::IsReady() const {
  if (!IsBound()) {
    return false;
  }

  return fragment_device() || !uses_fragment_driver_;
}

zx_status_t CompositeDeviceFragment::CreateProxy(const fbl::RefPtr<DriverHost> driver_host) {
  if (!IsReady()) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // If we've already created one, then don't redo work.
  if (proxy_device_) {
    return ZX_OK;
  }

  fbl::RefPtr<Device> parent = bound_device();

  // If the device we're bound to is proxied, we care about its proxy
  // rather than it, since that's the side that we communicate with.
  if (bound_device()->proxy()) {
    parent = bound_device()->proxy();
  }

  // Check if we need to create a proxy. If not, share a reference to
  // the instance of the fragment device.
  // We always use a proxy when there is an outgoing directory involved.
  if (parent->host() == driver_host && uses_fragment_driver()) {
    proxy_device_ = fragment_device_;
    return ZX_OK;
  }

  // Create a FIDL proxy.
  if (!uses_fragment_driver()) {
    VLOGF(1, "Preparing FIDL proxy for %s", parent->name().data());
    fbl::RefPtr<Device> fidl_proxy;
    auto status = parent->coordinator->PrepareFidlProxy(parent, driver_host, &fidl_proxy);
    if (status != ZX_OK) {
      return status;
    }
    proxy_device_ = fidl_proxy;
    return ZX_OK;
  }

  // Create a Banjo proxy.

  // We've already created our fragment's proxy, we're waiting for fragment.proxy.cm's device.
  if (!fragment_device()->fidl_proxies().empty()) {
    return ZX_ERR_SHOULD_WAIT;
  }

  // Double check that we haven't ended up in a state
  // where the proxies would need to be in different processes.
  if (driver_host && fragment_device() && fragment_device()->proxy() &&
      fragment_device()->proxy()->host() && fragment_device()->proxy()->host() != driver_host) {
    LOGF(ERROR, "Cannot create composite device, device proxies are in different driver_hosts");
    return ZX_ERR_BAD_STATE;
  }

  VLOGF(1, "Preparing Banjo proxy for %s", fragment_device()->name().data());
  // Here we create a FidlProxy in the composite device's driver host, and then
  // force-bind the fragment proxy driver to it.
  fbl::RefPtr<Device> fidl_proxy;
  auto status = parent->coordinator->PrepareFidlProxy(fragment_device(), driver_host, &fidl_proxy);
  if (status != ZX_OK) {
    return status;
  }
  status = parent->coordinator->AttemptBind(fdf::kFragmentProxyDriverInfo, fidl_proxy);
  if (status != ZX_OK) {
    return status;
  }
  // We have to wait until our fragment.proxy.cm device is created.
  return ZX_ERR_SHOULD_WAIT;
}

void CompositeDeviceFragment::Unbind() {
  ZX_ASSERT(bound_device_);
  composite_->UnbindFragment(this);

  // Drop our reference to any devices we've created.
  proxy_device_ = nullptr;
  fragment_device_ = nullptr;

  bound_device_->disassociate_from_composite();
  bound_device_ = nullptr;
}

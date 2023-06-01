// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/zx_device.h"

#include <lib/fit/defer.h>
#include <stdio.h>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "src/devices/bin/driver_host/composite_device.h"
#include "src/devices/bin/driver_host/driver_host.h"
#include "src/devices/bin/driver_host/fidl_proxy_device.h"
#include "src/devices/bin/driver_host/log.h"
#include "src/devices/lib/fidl/device_server.h"
#include "src/devices/lib/log/log.h"

zx_device::zx_device(DriverHostContext* ctx, std::string name, fbl::RefPtr<Driver> drv)
    : driver(drv), driver_ref_(drv.get()), driver_host_context_(ctx) {
  size_t len = name.length();
  // TODO(teisenbe): I think this is overly aggressive, and could be changed
  // to |len > ZX_DEVICE_NAME_MAX| and |len = ZX_DEVICE_NAME_MAX|.
  if (len >= ZX_DEVICE_NAME_MAX) {
    LOGF(WARNING, "Name too large for device %p: %s", this, name.c_str());
    len = ZX_DEVICE_NAME_MAX - 1;
    magic = 0;
  }

  memcpy(name_, name.data(), len);
  name_[len] = '\0';

  inspect_.emplace(driver->zx_driver()->inspect().devices(), name_);
}

zx_device::~zx_device() = default;

zx_status_t zx_device::Create(DriverHostContext* ctx, std::string name, fbl::RefPtr<Driver> driver,
                              fbl::RefPtr<zx_device>* out_dev) {
  *out_dev = fbl::AdoptRef(new zx_device(ctx, name, driver));
  (*out_dev)->vnode.emplace(**out_dev, ctx->loop().dispatcher());
  return ZX_OK;
}

void zx_device::set_bind_conn(fit::callback<void(zx_status_t)> conn) {
  fbl::AutoLock<fbl::Mutex> lock(&bind_conn_lock_);
  bind_conn_ = std::move(conn);
}

fit::callback<void(zx_status_t)> zx_device::take_bind_conn() {
  fbl::AutoLock<fbl::Mutex> lock(&bind_conn_lock_);
  auto conn = std::move(bind_conn_);
  bind_conn_ = nullptr;
  return conn;
}

void zx_device::set_rebind_conn(fit::callback<void(zx_status_t)> conn) {
  fbl::AutoLock<fbl::Mutex> lock(&rebind_conn_lock_);
  rebind_conn_ = std::move(conn);
}

void zx_device::call_rebind_conn_if_exists(zx_status_t status) {
  auto conn = [this]() {
    fbl::AutoLock<fbl::Mutex> lock(&rebind_conn_lock_);
    return std::exchange(rebind_conn_, nullptr);
  }();
  if (!conn) {
    return;
  }

  // DriverManager will return ZX_ERR_NOT_FOUND if it didn't find any drivers to bind.
  // We don't want to surface this error to the end user.
  if (status == ZX_ERR_NOT_FOUND) {
    status = ZX_OK;
  }
  conn(status);
}

void zx_device::set_unbind_children_conn(fit::callback<void(zx_status_t)> conn) {
  fbl::AutoLock<fbl::Mutex> lock(&unbind_children_conn_lock_);
  unbind_children_conn_ = std::move(conn);
}

fit::callback<void(zx_status_t)> zx_device::take_unbind_children_conn() {
  fbl::AutoLock<fbl::Mutex> lock(&unbind_children_conn_lock_);
  auto conn = std::move(unbind_children_conn_);
  unbind_children_conn_ = nullptr;
  return conn;
}

void zx_device::set_rebind_drv_name(std::string drv_name) {
  rebind_drv_name_ = std::move(drv_name);
}

const zx_device::DevicePowerStates& zx_device::GetPowerStates() const { return power_states_; }

const zx_device::PerformanceStates& zx_device::GetPerformanceStates() const {
  return performance_states_;
}

const zx_device::SystemPowerStateMapping& zx_device::GetSystemPowerStateMapping() const {
  return system_power_states_mapping_;
}

zx_status_t zx_device::SetPowerStates(const device_power_state_info_t* power_states,
                                      uint8_t count) {
  if (count < fuchsia_device::wire::kMinDevicePowerStates ||
      count > fuchsia_device::wire::kMaxDevicePowerStates) {
    return ZX_ERR_INVALID_ARGS;
  }
  bool visited[fuchsia_device::wire::kMaxDevicePowerStates] = {false};
  for (uint8_t i = 0; i < count; i++) {
    const auto& info = power_states[i];
    if (info.state_id >= std::size(visited)) {
      return ZX_ERR_INVALID_ARGS;
    }
    if (visited[info.state_id]) {
      return ZX_ERR_INVALID_ARGS;
    }
    auto state = &power_states_[info.state_id];
    state->state_id = static_cast<fuchsia_device::wire::DevicePowerState>(info.state_id);
    state->is_supported = true;
    state->restore_latency = info.restore_latency;
    state->wakeup_capable = info.wakeup_capable;
    state->system_wake_state = info.system_wake_state;
    visited[info.state_id] = true;
  }
  if (!(power_states_[static_cast<uint8_t>(
                          fuchsia_device::wire::DevicePowerState::kDevicePowerStateD0)]
            .is_supported) ||
      !(power_states_[static_cast<uint8_t>(
                          fuchsia_device::wire::DevicePowerState::kDevicePowerStateD3Cold)]
            .is_supported)) {
    return ZX_ERR_INVALID_ARGS;
  }
  inspect_->set_power_states(power_states, count);
  return ZX_OK;
}

zx_status_t zx_device::SetPerformanceStates(
    const device_performance_state_info_t* performance_states, uint8_t count) {
  if (count < fuchsia_device::wire::kMinDevicePerformanceStates ||
      count > fuchsia_device::wire::kMaxDevicePerformanceStates) {
    return ZX_ERR_INVALID_ARGS;
  }
  bool visited[fuchsia_device::wire::kMaxDevicePerformanceStates] = {false};
  for (uint8_t i = 0; i < count; i++) {
    const auto& info = performance_states[i];
    if (info.state_id >= std::size(visited)) {
      return ZX_ERR_INVALID_ARGS;
    }
    if (visited[info.state_id]) {
      return ZX_ERR_INVALID_ARGS;
    }
    fuchsia_device::wire::DevicePerformanceStateInfo* state = &(performance_states_[info.state_id]);
    state->state_id = info.state_id;
    state->is_supported = true;
    state->restore_latency = info.restore_latency;
    visited[info.state_id] = true;
  }
  if (!(performance_states_[fuchsia_device::wire::kDevicePerformanceStateP0].is_supported)) {
    return ZX_ERR_INVALID_ARGS;
  }
  inspect_->set_performance_states(performance_states, count);
  return ZX_OK;
}

namespace {

using fuchsia_device_manager::wire::SystemPowerState;

uint8_t get_suspend_reason(SystemPowerState power_state) {
  switch (power_state) {
    case SystemPowerState::kReboot:
      return DEVICE_SUSPEND_REASON_REBOOT;
    case SystemPowerState::kRebootRecovery:
      return DEVICE_SUSPEND_REASON_REBOOT_RECOVERY;
    case SystemPowerState::kRebootBootloader:
      return DEVICE_SUSPEND_REASON_REBOOT_BOOTLOADER;
    case SystemPowerState::kMexec:
      return DEVICE_SUSPEND_REASON_MEXEC;
    case SystemPowerState::kPoweroff:
      return DEVICE_SUSPEND_REASON_POWEROFF;
    case SystemPowerState::kSuspendRam:
      return DEVICE_SUSPEND_REASON_SUSPEND_RAM;
    case SystemPowerState::kRebootKernelInitiated:
      return DEVICE_SUSPEND_REASON_REBOOT_KERNEL_INITIATED;
    default:
      return DEVICE_SUSPEND_REASON_SELECTIVE_SUSPEND;
  }
}
}  // namespace

zx_status_t zx_device_t::get_dev_power_state_from_mapping(
    uint32_t flags, fuchsia_device::wire::SystemPowerStateInfo* info, uint8_t* suspend_reason) {
  // TODO(fxbug.dev/109243) : When the usage of suspend flags is replaced with system power states,
  // this function will not need the switch case. Some suspend flags might be translated to system
  // power states with additional hints (ex: REBOOT/REBOOT_BOOTLOADER/REBOOT_RECOVERY/MEXEC). For
  // now, each of these flags are treated as an individual state.
  SystemPowerState sys_state;
  switch (flags) {
    case DEVICE_SUSPEND_FLAG_REBOOT:
      sys_state = SystemPowerState::kReboot;
      break;
    case DEVICE_SUSPEND_FLAG_REBOOT_RECOVERY:
      sys_state = SystemPowerState::kRebootRecovery;
      break;
    case DEVICE_SUSPEND_FLAG_REBOOT_BOOTLOADER:
      sys_state = SystemPowerState::kRebootBootloader;
      break;
    case DEVICE_SUSPEND_FLAG_MEXEC:
      sys_state = SystemPowerState::kMexec;
      break;
    case DEVICE_SUSPEND_FLAG_POWEROFF:
      sys_state = SystemPowerState::kPoweroff;
      break;
    case DEVICE_SUSPEND_FLAG_SUSPEND_RAM:
      sys_state = SystemPowerState::kSuspendRam;
      break;
    case DEVICE_SUSPEND_FLAG_REBOOT_KERNEL_INITIATED:
      sys_state = SystemPowerState::kRebootKernelInitiated;
      break;
    default:
      return ZX_ERR_INVALID_ARGS;
  }

  auto& sys_power_states = GetSystemPowerStateMapping();

  // SystemPowerState (from FIDL) use a 1-based index, so subtract 1 for indexing into the array
  auto sys_power_idx = static_cast<unsigned long>(sys_state) - 1;

  *info = sys_power_states.at(sys_power_idx);
  *suspend_reason = get_suspend_reason(sys_state);
  return ZX_OK;
}

zx_status_t zx_device::SetSystemPowerStateMapping(const SystemPowerStateMapping& mapping) {
  for (size_t i = 0; i < mapping.size(); i++) {
    auto info = &mapping[i];
    if (!power_states_[static_cast<uint8_t>(info->dev_state)].is_supported) {
      return ZX_ERR_INVALID_ARGS;
    }
    if (info->wakeup_enable &&
        !power_states_[static_cast<uint8_t>(info->dev_state)].wakeup_capable) {
      return ZX_ERR_INVALID_ARGS;
    }
    // TODO(ravoorir): Validate whether the system can wake up from that state,
    // when power states make more sense. Currently we cannot compare the
    // system sleep power states.
    system_power_states_mapping_[i] = mapping[i];
  }
  inspect_->set_system_power_state_mapping(mapping);
  return ZX_OK;
}

// We must disable thread-safety analysis due to not being able to statically
// guarantee the lock holding invariant.  Instead, we acquire the lock if
// it's not already being held by the current thread.
void zx_device::fbl_recycle() TA_NO_THREAD_SAFETY_ANALYSIS {
  bool acq_lock = !driver_host_context_->api_lock().IsHeldByCurrentThread();
  if (acq_lock) {
    driver_host_context_->api_lock().Acquire();
  }
  auto unlock = fit::defer([this, acq_lock]() TA_NO_THREAD_SAFETY_ANALYSIS {
    if (acq_lock) {
      driver_host_context_->api_lock().Release();
    }
  });

  if (this->flags() & DEV_FLAG_BUSY) {
    // this can happen if creation fails
    // the caller to device_add() will free it
    LOGD(WARNING, *this, "Not releasing device %p, it is busy", this);
    return;
  }
  VLOGD(1, *this, "Releasing device %p", this);

  if (!(this->flags() & DEV_FLAG_DEAD)) {
    LOGD(WARNING, *this, "Releasing device %p which is not yet dead", this);
  }
  if (!this->children().is_empty()) {
    LOGD(WARNING, *this, "Releasing device %p which still has children", this);
  }

  composite_.reset();
  fidl_proxy_.reset();

  driver_host_context_->QueueDeviceForFinalization(this);
}

static fbl::Mutex local_id_map_lock_;
static fbl::TaggedWAVLTree<uint64_t, fbl::RefPtr<zx_device>, zx_device::LocalIdMapTag,
                           zx_device::LocalIdKeyTraits>
    local_id_map_ TA_GUARDED(local_id_map_lock_);

void zx_device::set_local_id(uint64_t id) {
  // If this is the last reference, we want it to go away outside of the lock
  fbl::RefPtr<zx_device> old_entry;

  fbl::AutoLock guard(&local_id_map_lock_);
  if (local_id_ != 0) {
    old_entry = local_id_map_.erase(*this);
    ZX_ASSERT(old_entry.get() == this);
  }

  local_id_ = id;
  if (id != 0) {
    local_id_map_.insert(fbl::RefPtr(this));
  }
  inspect_->set_local_id(id);

  // Update parent local id all inspect data of children.
  // This is needed because sometimes parent local id is set after the children are created.
  for (auto& child : children_) {
    child.inspect().set_parent(fbl::RefPtr(this));
  }
}

fbl::RefPtr<zx_device> zx_device::GetDeviceFromLocalId(uint64_t local_id) {
  fbl::AutoLock guard(&local_id_map_lock_);
  auto itr = local_id_map_.find(local_id);
  if (itr == local_id_map_.end()) {
    return nullptr;
  }
  return fbl::RefPtr(&*itr);
}

bool zx_device::has_composite() const { return !!composite_; }

fbl::RefPtr<CompositeDevice> zx_device::take_composite() { return std::move(composite_); }

void zx_device::set_composite(fbl::RefPtr<CompositeDevice> composite, bool fragment) {
  composite_ = std::move(composite);
  is_composite_ = !fragment;
  if (fragment) {
    inspect_->set_fragment();
  } else {
    inspect_->set_composite();
  }
}

bool zx_device::is_composite() const { return is_composite_ && !!composite_; }

fbl::RefPtr<CompositeDevice> zx_device::composite() { return composite_; }

void zx_device::set_fidl_proxy(fbl::RefPtr<FidlProxyDevice> fidl_proxy) {
  fidl_proxy_ = std::move(fidl_proxy);
  is_fidl_proxy_ = true;
  inspect_->set_fidl_proxy();
}

bool zx_device::is_fidl_proxy() const { return is_fidl_proxy_ && !!fidl_proxy_; }

fbl::RefPtr<FidlProxyDevice> zx_device::fidl_proxy() { return fidl_proxy_; }

bool zx_device::IsPerformanceStateSupported(uint32_t requested_state) {
  if (requested_state >= fuchsia_device::wire::kMaxDevicePerformanceStates) {
    return false;
  }
  return performance_states_[requested_state].is_supported;
}

void zx_device::add_child(zx_device* child) {
  children_.push_back(child);
  inspect_->increment_child_count();
}
void zx_device::remove_child(zx_device& child) {
  children_.erase(child);
  inspect_->decrement_child_count();
}

zx::result<std::string> zx_device::GetTopologicalPath() {
  char buf[fuchsia_device::wire::kMaxDevicePathLen + 1];
  size_t actual;
  if (zx_status_t status =
          driver_host_context()->GetTopoPath(fbl::RefPtr(this), buf, sizeof(buf), &actual);
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::string(buf));
}

void zx_device::LogError(const char* error) {
  zx::result topo_path = GetTopologicalPath();
  LOGF(ERROR, "%s (%d): %s",
       topo_path.is_ok() ? topo_path.value().c_str() : topo_path.status_string(), protocol_id(),
       error);
}

bool zx_device::IsUnbound() { return flags_ & DEV_FLAG_UNBINDING; }

bool zx_device::MessageOp(fidl::IncomingHeaderAndMessage msg, device_fidl_txn_t txn) {
  if (!ops_.message) {
    return false;
  }

  libsync::Completion completion;

  async::PostTask(driver->dispatcher()->async_dispatcher(), [&]() {
    TraceLabelBuffer trace_label;
    TRACE_DURATION("driver_host:driver-hooks", get_trace_label("message", &trace_label));
    inspect_->MessageOpStats().Update();
    ops_.message(ctx(), std::move(msg).ReleaseToEncodedCMessage(), txn);
    completion.Signal();
  });

  completion.Wait();
  return true;
}

zx_status_t zx_device::Rebind() {
  DriverHostContext& context = *driver_host_context();
  fbl::AutoLock lock(&context.api_lock());
  zx::result scheduled_unbind = context.ScheduleUnbindChildren(fbl::RefPtr(this));
  if (scheduled_unbind.is_error()) {
    return scheduled_unbind.error_value();
  }
  // This will be true if we had children, we will want to wait to rebind until our children are
  // gone.
  if (scheduled_unbind.value()) {
    set_flag(DEV_FLAG_WANTS_REBIND);
    return ZX_OK;
  }

  // We don't have any children, so try to rebind right now.
  zx_status_t status = context.DeviceBind(fbl::RefPtr(this), get_rebind_drv_name().c_str());
  if (status != ZX_OK) {
    // Since device binding didn't work, we should reply to an outstanding rebind if it exists;
    call_rebind_conn_if_exists(status);
  }
  return status;
}

void zx_device::ConnectToDeviceFidl(ConnectToDeviceFidlRequestView request,
                                    ConnectToDeviceFidlCompleter::Sync& completer) {
  if (vnode.has_value()) {
    vnode.value().ConnectToDeviceFidl(std::move(request->server));
  }
}

void zx_device::ConnectToController(ConnectToControllerRequestView request,
                                    ConnectToControllerCompleter::Sync& completer) {
  if (vnode.has_value()) {
    vnode.value().ConnectToController(std::move(request->server));
  }
}

void zx_device::Bind(BindRequestView request, BindCompleter::Sync& completer) {
  zx_status_t status = device_bind(fbl::RefPtr(this), std::string(request->driver.get()).c_str());
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  set_bind_conn([completer = completer.ToAsync()](zx_status_t status) mutable {
    completer.Reply(zx::make_result(status));
  });
}

void zx_device::GetCurrentPerformanceState(GetCurrentPerformanceStateCompleter::Sync& completer) {
  completer.Reply(current_performance_state());
}

void zx_device::Rebind(RebindRequestView request, RebindCompleter::Sync& completer) {
  set_rebind_drv_name(std::string(request->driver.get()));
  // This will be called after the device is rebound. If DriverManager finds a driver for this
  // device, this will be called after the new driver has been bound and has created a new device.
  set_rebind_conn([completer = completer.ToAsync()](zx_status_t status) mutable {
    completer.Reply(zx::make_result(status));
  });
  // This function will always result in a call to the rebind connector callback.
  std::ignore = Rebind();
}

void zx_device::UnbindChildren(UnbindChildrenCompleter::Sync& completer) {
  zx::result<bool> scheduled_unbind = device_schedule_unbind_children(fbl::RefPtr(this));
  if (scheduled_unbind.is_error()) {
    completer.ReplyError(scheduled_unbind.status_value());
    return;
  }

  // Handle case where we have no children to unbind (otherwise the callback below will never
  // fire).
  if (!scheduled_unbind.value()) {
    completer.ReplySuccess();
    return;
  }

  // Asynchronously respond to the unbind request once all children have been unbound.
  // The unbind children conn will be set until all the children of this device are unbound.
  set_unbind_children_conn([completer = completer.ToAsync()](zx_status_t status) mutable {
    completer.Reply(zx::make_result(status));
  });
}

void zx_device::ScheduleUnbind(ScheduleUnbindCompleter::Sync& completer) {
  zx_status_t status = device_schedule_remove(fbl::RefPtr(this), true /* unbind_self */);
  completer.Reply(zx::make_result(status));
}

void zx_device::GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) {
  zx::result topo_path = GetTopologicalPath();
  if (topo_path.is_error()) {
    completer.ReplyError(topo_path.error_value());
    return;
  }
  completer.ReplySuccess(fidl::StringView::FromExternal(topo_path.value()));
}

void zx_device::GetMinDriverLogSeverity(GetMinDriverLogSeverityCompleter::Sync& completer) {
  if (!driver) {
    completer.Reply(ZX_ERR_UNAVAILABLE, fuchsia_logger::wire::LogLevelFilter::kNone);
    return;
  }
  fx_log_severity_t severity = zx_driver()->logger().GetSeverity();
  completer.Reply(ZX_OK, static_cast<fuchsia_logger::wire::LogLevelFilter>(severity));
}

void zx_device::SetMinDriverLogSeverity(SetMinDriverLogSeverityRequestView request,
                                        SetMinDriverLogSeverityCompleter::Sync& completer) {
  if (!driver) {
    completer.Reply(ZX_ERR_UNAVAILABLE);
    return;
  }
  auto status =
      zx_driver()->set_driver_min_log_severity(static_cast<fx_log_severity_t>(request->severity));
  completer.Reply(status);
}

void zx_device::SetPerformanceState(SetPerformanceStateRequestView request,
                                    SetPerformanceStateCompleter::Sync& completer) {
  uint32_t out_state;
  zx_status_t status = driver_host_context()->DeviceSetPerformanceState(
      fbl::RefPtr(this), request->requested_state, &out_state);
  completer.Reply(status, out_state);
}

// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/device.h"

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.driver.test.logger/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fidl/coding.h>
#include <lib/fit/defer.h>
#include <lib/zx/clock.h>
#include <zircon/status.h>

#include <deque>
#include <memory>
#include <string_view>

#include <fbl/string_buffer.h>

#include "src/devices/bin/driver_manager/coordinator.h"
#include "src/devices/bin/driver_manager/devfs/devfs.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/join_strings.h"
#include "src/lib/fxl/strings/utf_codecs.h"

// TODO(fxbug.dev/43370): remove this once init tasks can be enabled for all devices.
static constexpr bool kEnableAlwaysInit = false;

namespace {

bool StringHasPrefix(std::string_view prefix, std::string_view str) {
  return str.find(prefix, 0) == 0;
}

std::string StateToString(Device::State state) {
  switch (state) {
    case Device::State::kActive:
      return "kActive";
    case Device::State::kInitializing:
      return "kInitializing";
    case Device::State::kSuspending:
      return "kSuspending";
    case Device::State::kSuspended:
      return "kSuspended";
    case Device::State::kResuming:
      return "kResuming";
    case Device::State::kResumed:
      return "kResumed";
    case Device::State::kUnbinding:
      return "kUnbinding";
    case Device::State::kDead:
      return "kDead";
  }
}

}  // namespace

Device::Device(Coordinator* coord, fbl::String name, fbl::String parent_driver_url,
               fbl::RefPtr<Device> parent, uint32_t protocol_id, zx::vmo inspect,
               fidl::ClientEnd<fuchsia_device_manager::DeviceController> device_controller,
               fidl::ClientEnd<fio::Directory> outgoing_dir)
    : coordinator(coord),
      name_(std::move(name)),
      parent_driver_url_(std::move(parent_driver_url)),
      parent_(std::move(parent)),
      protocol_id_(protocol_id),
      publish_task_([this] {
        // TODO(tesienbe): We probably should do something with the return value
        // from this...
        coordinator->bind_driver_manager().BindDevice(fbl::RefPtr(this));
      }),
      outgoing_dir_(std::move(outgoing_dir)),
      inspect_(
          coord->inspect_manager().CreateDevice(name_.c_str(), std::move(inspect), protocol_id_)) {
  if (device_controller.is_valid()) {
    device_controller_.Bind(std::move(device_controller), coordinator->dispatcher());
  }
  set_state(Device::State::kActive);  // set default state
}

Device::~Device() {
  // Ideally we'd assert here that immortal devices are never destroyed, but
  // they're destroyed when the Coordinator object is cleaned up in tests.
  // We can probably get rid of the IMMORTAL flag, since if the Coordinator is
  // holding a reference we shouldn't be able to hit that check, in which case
  // the flag is only used to modify the proxy library loading behavior.
  devfs.unpublish();

  // Drop our reference to our driver_host if we still have it
  set_host(nullptr);

  // TODO: cancel any pending rpc responses
  // TODO: Have dtor assert that DEV_CTX_IMMORTAL set on flags
  VLOGF(1, "Destroyed device %p '%s'", this, name_.data());
}

void Device::Serve(fidl::ServerEnd<fuchsia_device_manager::Coordinator> request) {
  coordinator_binding_ = fidl::BindServer(
      coordinator->dispatcher(), std::move(request), this,
      [dev = fbl::RefPtr<Device>(this)](
          Device* self, fidl::UnbindInfo info,
          fidl::ServerEnd<fuchsia_device_manager::Coordinator> server_end) {
        if (info.is_user_initiated()) {
          return;
        }
        if (info.is_peer_closed()) {
          // If the device is already dead, we are detecting an expected disconnect from the
          // driver_host.
          if (dev->state() != Device::State::kDead) {
            // TODO(https://fxbug.dev/56208): Change this log back to error once isolated devmgr
            // is fixed.
            LOGF(WARNING, "Disconnected device %p '%s', see fxbug.dev/56208 for potential cause",
                 dev.get(), dev->name().data());

            dev->coordinator->device_manager()->RemoveDevice(dev, true);
          }
          return;
        }
        LOGF(ERROR, "Failed to handle RPC for device %p '%s': %s", dev.get(), dev->name().data(),
             info.FormatDescription().c_str());
      });
}

zx_status_t Device::Create(
    Coordinator* coordinator, const fbl::RefPtr<Device>& parent, fbl::String name,
    fbl::String parent_driver_url, uint32_t protocol_id, fbl::Array<zx_device_prop_t> props,
    fbl::Array<StrProperty> str_props,
    fidl::ServerEnd<fuchsia_device_manager::Coordinator> coordinator_request,
    fidl::ClientEnd<fuchsia_device_manager::DeviceController> device_controller,
    bool want_init_task, fuchsia_device_manager::wire::AddDeviceConfig add_device_config,
    zx::vmo inspect, fidl::ClientEnd<fio::Directory> outgoing_dir, fbl::RefPtr<Device>* device) {
  fbl::RefPtr<Device> real_parent = parent;
  // If our parent is a proxy, for the purpose of devfs, we need to work with
  // *its* parent which is the device that it is proxying.
  if (real_parent->flags & DEV_CTX_PROXY) {
    real_parent = real_parent->parent();
  }

  auto dev = fbl::MakeRefCounted<Device>(coordinator, std::move(name), std::move(parent_driver_url),
                                         real_parent, protocol_id, std::move(inspect),
                                         std::move(device_controller), std::move(outgoing_dir));
  if (!dev) {
    return ZX_ERR_NO_MEMORY;
  }

  // If the device was created by the proxy driver then it's a proxy.
  if (dev->is_fragment_proxy_device()) {
    dev->flags |= DEV_CTX_PROXY;
    if (real_parent->proxy()) {
      return ZX_ERR_ALREADY_EXISTS;
    }
  }

  if (add_device_config & fuchsia_device_manager::AddDeviceConfig::kSkipAutobind) {
    dev->flags |= DEV_CTX_SKIP_AUTOBIND;
  }

  if (add_device_config & fuchsia_device_manager::AddDeviceConfig::kMustIsolate) {
    dev->flags |= DEV_CTX_MUST_ISOLATE;
  }

  if (add_device_config & fuchsia_device_manager::wire::AddDeviceConfig::kAllowMultiComposite) {
    dev->flags |= DEV_CTX_ALLOW_MULTI_COMPOSITE;
  }

  if (zx::result status = dev->inspect_.Publish(); status.is_error()) {
    return status.error_value();
  }

  zx_status_t status = dev->SetProps(std::move(props));
  if (status != ZX_OK) {
    return status;
  }

  if (auto status = dev->SetStrProps(std::move(str_props)); status != ZX_OK) {
    return status;
  }

  dev->Serve(std::move(coordinator_request));

  // We exist within our parent's device host
  dev->set_host(parent->host());

  // We must mark the device as invisible before publishing so
  // that we don't send "device added" notifications.
  // The init task must complete before marking the device visible.
  if (want_init_task) {
    dev->flags |= DEV_CTX_INVISIBLE;
  }

  status = dev->InitializeToDevfs();
  if (status != ZX_OK) {
    return status;
  }

  if (!(dev->flags & DEV_CTX_PROXY)) {
    real_parent->children_.push_back(dev.get());
    VLOGF(1, "Created device %p '%s' (child of %p '%s')", dev.get(), dev->name().c_str(),
          real_parent.get(), real_parent->name().c_str());
  } else {
    // We should've already verified that this is null by this point.
    ZX_ASSERT_MSG(real_parent->proxy_ == nullptr, "Trying to add a second proxy to device %s",
                  real_parent->parent_driver_url().c_str());
    real_parent->proxy_ = dev;
  }

  if (want_init_task) {
    dev->CreateInitTask();
  }

  dev->InitializeInspectValues();

  *device = std::move(dev);
  return ZX_OK;
}

zx_status_t Device::CreateComposite(
    Coordinator* coordinator, fbl::RefPtr<DriverHost> driver_host, CompositeDevice& composite,
    fidl::ServerEnd<fuchsia_device_manager::Coordinator> coordinator_request,
    fidl::ClientEnd<fuchsia_device_manager::DeviceController> device_controller,
    fbl::RefPtr<Device>* device) {
  const auto& composite_props = composite.properties();
  fbl::Array<zx_device_prop_t> props(new zx_device_prop_t[composite_props.size()],
                                     composite_props.size());
  memcpy(props.data(), composite_props.data(), props.size() * sizeof(props[0]));

  const auto& composite_str_props = composite.str_properties();
  fbl::Array<StrProperty> str_props(new StrProperty[composite_str_props.size()],
                                    composite_str_props.size());
  for (size_t i = 0; i < composite_str_props.size(); i++) {
    str_props[i] = composite_str_props[i];
  }

  fbl::RefPtr<Device> real_parent = composite.GetPrimaryFragment()->bound_device();
  // If our parent is a proxy, for the purpose of devfs, we need to work with
  // *its* parent which is the device that it is proxying.
  if (real_parent->flags & DEV_CTX_PROXY) {
    real_parent = real_parent->parent();
  }

  auto dev = fbl::MakeRefCounted<Device>(coordinator, composite.name(), fbl::String(), real_parent,
                                         0, zx::vmo(), std::move(device_controller),
                                         fidl::ClientEnd<fio::Directory>());
  if (!dev) {
    return ZX_ERR_NO_MEMORY;
  }
  dev->composite_ = composite;

  if (zx::result status = dev->inspect_.Publish(); status.is_error()) {
    return status.error_value();
  }

  zx_status_t status = dev->SetProps(std::move(props));
  if (status != ZX_OK) {
    return status;
  }

  status = dev->SetStrProps(std::move(str_props));
  if (status != ZX_OK) {
    return status;
  }

  dev->Serve(std::move(coordinator_request));
  // We exist within our parent's device host
  dev->set_host(std::move(driver_host));

  status = dev->InitializeToDevfs();
  if (status != ZX_OK) {
    return status;
  }

  real_parent->children_.push_back(dev.get());
  VLOGF(1, "Created composite device %p '%s' (child of %p '%s')", dev.get(), dev->name().c_str(),
        real_parent.get(), real_parent->name().c_str());

  dev->InitializeInspectValues();
  *device = std::move(dev);
  return ZX_OK;
}

zx_status_t Device::CreateProxy(
    fidl::ClientEnd<fuchsia_device_manager::DeviceController> controller) {
  ZX_ASSERT(proxy_ == nullptr);

  fbl::String driver_path = parent_driver_url_;
  // non-immortal devices, use foo.proxy.cm for
  // their proxy devices instead of foo.so
  if (!(this->flags & DEV_CTX_IMMORTAL)) {
    const char* begin = driver_path.data();
    const char* end = strstr(begin, ".cm");
    std::string_view prefix(begin, end == nullptr ? driver_path.size() : end - begin);
    driver_path = fbl::String::Concat({prefix, ".proxy.cm"});
  }

  auto dev = fbl::MakeRefCounted<Device>(this->coordinator, fbl::String::Concat({name_, "-proxy"}),
                                         std::move(driver_path), fbl::RefPtr(this), protocol_id_,
                                         zx::vmo(), std::move(controller),
                                         fidl::ClientEnd<fio::Directory>());
  if (dev == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }

  dev->flags = DEV_CTX_PROXY;

  dev->InitializeInspectValues();

  proxy_ = std::move(dev);
  VLOGF(1, "Created proxy device %p '%s'", this, name_.data());
  return ZX_OK;
}

zx_status_t Device::CreateFidlProxy(
    fidl::ClientEnd<fuchsia_device_manager::DeviceController> controller,
    fbl::RefPtr<Device>* fidl_proxy_out) {
  auto dev = fbl::MakeRefCounted<Device>(
      this->coordinator, fbl::String::Concat({name_, "-fidl-proxy"}), fbl::String(),
      fbl::RefPtr(this), 0, zx::vmo(), std::move(controller), fidl::ClientEnd<fio::Directory>());
  if (dev == nullptr) {
    return ZX_ERR_NO_MEMORY;
  }

  dev->flags = DEV_CTX_PROXY;

  dev->InitializeInspectValues();

  *fidl_proxy_out = dev;
  fidl_proxies_.push_back(std::move(dev));
  VLOGF(1, "Created fidl_proxy device %p '%s'", this, name_.data());
  return ZX_OK;
}

void Device::set_state(Device::State state) {
  state_ = state;
  inspect_.set_state(StateToString(state));

  if (state == Device::State::kDead) {
    if (std::optional binding = std::exchange(coordinator_binding_, std::nullopt);
        binding.has_value()) {
      binding.value().Unbind();
    }
  }
}

void Device::InitializeInspectValues() {
  std::string type("Device");
  if (flags & DEV_CTX_PROXY) {
    type = std::string("Proxy device");
  } else if (is_composite()) {
    type = std::string("Composite device");
  }
  inspect_.SetStaticValues(MakeTopologicalPath(), protocol_id_, type, flags,
                           cpp20::span<const zx_device_prop_t>{props().data(), props().size()},
                           parent_driver_url());
}

void Device::DetachFromParent() {
  // Do this first as we might be deleting the last reference to ourselves.
  auto parent = std::move(parent_);
  if (this->flags & DEV_CTX_PROXY) {
    parent->proxy_ = nullptr;
  } else {
    parent->children_.erase(*this);
  }
}

template <typename DeviceType>
std::list<DeviceType*> Device::GetChildren(DeviceType* device) {
  std::list<DeviceType*> children;

  // Add our obvious children.
  for (auto& child : device->children_) {
    children.push_back(&child);
  }

  // If we are a fragment device, we can find more children by looking
  // at our parent's fragment list.
  if (device->is_fragment_device()) {
    if (!device->parent_) {
      return children;
    }
    for (auto& fragment : device->parent_->fragments_) {
      // Skip composite devices that aren't bound.
      if (fragment.composite()->device() == nullptr) {
        continue;
      }
      // Skip fragments that have a frament device.
      if (fragment.fragment_device().get() != device) {
        continue;
      }
      children.push_back(fragment.composite()->device().get());
    }
    return children;
  }

  // Some composite devices are added directly as fragments without
  // a proxy fragment device. These don't appear in the children list.
  for (auto& fragment : device->fragments_) {
    // Skip composite devices that aren't bound.
    if (fragment.composite()->device() == nullptr) {
      continue;
    }
    // Skip fragments that have a fragment device.
    if (fragment.fragment_device() != nullptr) {
      continue;
    }
    children.push_back(fragment.composite()->device().get());
  }
  return children;
}

std::list<const Device*> Device::children() const {
  return Device::GetChildren<const Device>(this);
}

std::list<Device*> Device::children() { return Device::GetChildren<Device>(this); }

std::string Device::MakeTopologicalPath() const {
  std::deque<std::string_view> names;
  const Device* dev = this;
  while (dev != nullptr) {
    names.push_front(dev->name());
    dev = dev->parent().get();
    if (dev && dev->flags & DEV_CTX_PROXY) {
      dev = dev->parent().get();
    }
  }
  names.push_front("/dev");
  return fxl::JoinStrings(names, "/");
}

zx_status_t Device::InitializeToDevfs() {
  ZX_ASSERT(parent_ != nullptr);
  Device& parent = *parent_;
  if (!parent.devfs.topological_node().has_value() || devfs.topological_node().has_value() ||
      devfs.protocol_node().has_value()) {
    return ZX_ERR_INTERNAL;
  }

  zx_status_t status = parent.devfs.topological_node()->add_child(
      name_, ProtocolIdToClassName(protocol_id_), MakeDevfsTarget(), devfs);
  if (status != ZX_OK) {
    return status;
  }
  if (!(flags & DEV_CTX_INVISIBLE)) {
    devfs.publish();
  }
  return ZX_OK;
}

Devnode::Target Device::MakeDevfsTarget() {
  if (!device_controller_.is_valid()) {
    return Devnode::Target();
  }
  return Devnode::PassThrough([controller = device_controller_.Clone()](
                                  zx::channel connection,
                                  Devnode::PassThrough::ConnectionType type) {
    if (!controller.is_valid()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
    if (type.include_controller && !type.include_device && !type.include_node) {
      return controller
          ->ConnectToController(fidl::ServerEnd<fuchsia_device::Controller>(std::move(connection)))
          .status();
    }
    if (!type.include_controller && type.include_device && !type.include_node) {
      return controller->ConnectToDeviceProtocol(std::move(connection)).status();
    }
    return controller
        ->ConnectMultiplexed(std::move(connection), type.include_node, type.include_controller)
        .status();
  });
}

zx_status_t Device::SignalReadyForBind(zx::duration delay) {
  return publish_task_.PostDelayed(this->coordinator->dispatcher(), delay);
}

void Device::CreateInitTask() {
  // We only ever create an init task when a device is initially added.
  ZX_ASSERT(!active_init_);
  set_state(Device::State::kInitializing);
  active_init_ = InitTask::Create(fbl::RefPtr(this));
}

fbl::RefPtr<SuspendTask> Device::RequestSuspendTask(uint32_t suspend_flags) {
  if (active_suspend_) {
    // We don't support different types of suspends concurrently, and
    // shouldn't be able to reach this state.
    ZX_ASSERT(suspend_flags == active_suspend_->suspend_flags());
  } else {
    active_suspend_ = SuspendTask::Create(fbl::RefPtr(this), suspend_flags);
  }
  return active_suspend_;
}

void Device::SendInit(InitCompletion completion) {
  ZX_ASSERT(!init_completion_);

  VLOGF(1, "Initializing device %p '%s'", this, name_.data());
  device_controller()->Init().ThenExactlyOnce(
      [dev = fbl::RefPtr(this)](
          fidl::WireUnownedResult<fuchsia_device_manager::DeviceController::Init>& result) {
        if (!result.ok()) {
          dev->CompleteInit(result.status());
          return;
        }
        auto* response = result.Unwrap();
        VLOGF(1, "Initialized device %p '%s': %s", dev.get(), dev->name().data(),
              zx_status_get_string(response->status));
        dev->CompleteInit(response->status);
      });
  init_completion_ = std::move(completion);
}

zx_status_t Device::CompleteInit(zx_status_t status) {
  if (!init_completion_ && status == ZX_OK) {
    LOGF(ERROR, "Unexpected reply when initializing device %p '%s'", this, name_.data());
    return ZX_ERR_IO;
  }
  if (init_completion_) {
    init_completion_(status);
  }
  DropInitTask();
  return ZX_OK;
}

fbl::RefPtr<ResumeTask> Device::RequestResumeTask(uint32_t target_system_state) {
  if (active_resume_) {
    // We don't support different types of resumes concurrently, and
    // shouldn't be able to reach this state.
    ZX_ASSERT(target_system_state == active_resume_->target_system_state());
  } else {
    active_resume_ = ResumeTask::Create(fbl::RefPtr(this), target_system_state);
  }
  return active_resume_;
}

void Device::SendSuspend(uint32_t flags, SuspendCompletion completion) {
  if (suspend_completion_) {
    // We already have a pending suspend
    return completion(ZX_ERR_UNAVAILABLE);
  }
  VLOGF(1, "Suspending device %p '%s'", this, name_.data());
  device_controller()->Suspend(flags).ThenExactlyOnce(
      [dev = fbl::RefPtr(this)](
          fidl::WireUnownedResult<fuchsia_device_manager::DeviceController::Suspend>& result) {
        if (!result.ok()) {
          dev->CompleteSuspend(result.status());
          return;
        }
        auto* response = result.Unwrap();
        if (response->status == ZX_OK) {
          LOGF(DEBUG, "Suspended device %p '%s' successfully", dev.get(), dev->name().data());
        } else {
          LOGF(WARNING, "Failed to suspended device %p '%s': %s", dev.get(), dev->name().data(),
               zx_status_get_string(response->status));
        }
        dev->CompleteSuspend(response->status);
      });
  set_state(Device::State::kSuspending);
  suspend_completion_ = std::move(completion);
}

void Device::SendResume(uint32_t target_system_state, ResumeCompletion completion) {
  if (resume_completion_) {
    // We already have a pending resume
    return completion(ZX_ERR_UNAVAILABLE);
  }
  VLOGF(1, "Resuming device %p '%s'", this, name_.data());

  device_controller()
      ->Resume(target_system_state)
      .ThenExactlyOnce(
          [dev = fbl::RefPtr(this)](
              fidl::WireUnownedResult<fuchsia_device_manager::DeviceController::Resume>& result) {
            if (!result.ok()) {
              dev->CompleteResume(result.status());
              return;
            }
            auto* response = result.Unwrap();
            LOGF(INFO, "Resumed device %p '%s': %s", dev.get(), dev->name().data(),
                 zx_status_get_string(response->status));
            dev->CompleteResume(response->status);
          });
  set_state(Device::State::kResuming);
  resume_completion_ = std::move(completion);
}

void Device::CompleteSuspend(zx_status_t status) {
  // If a device is being removed, any existing suspend task will be forcibly completed,
  // in which case we should not update the state.
  if (state_ != Device::State::kDead) {
    if (status == ZX_OK) {
      set_state(Device::State::kSuspended);
    } else {
      set_state(Device::State::kActive);
    }
  }

  if (suspend_completion_) {
    suspend_completion_(status);
  }
  DropSuspendTask();
}

void Device::CompleteResume(zx_status_t status) {
  if (status == ZX_OK) {
    set_state(Device::State::kResumed);
  } else {
    set_state(Device::State::kSuspended);
  }
  if (resume_completion_) {
    resume_completion_(status);
  }
}

void Device::CreateUnbindRemoveTasks(UnbindTaskOpts opts) {
  if (state_ == Device::State::kDead) {
    return;
  }
  // Create the tasks if they do not exist yet. We always create both.
  if (active_unbind_ == nullptr && active_remove_ == nullptr) {
    // Make sure the remove task exists before the unbind task,
    // as the unbind task adds the remove task as a dependent.
    active_remove_ = RemoveTask::Create(fbl::RefPtr(this));
    active_unbind_ = UnbindTask::Create(fbl::RefPtr(this), opts);
    return;
  }
  if (!active_unbind_) {
    // The unbind task has already completed and the device is now being removed.
    return;
  }
  // User requested removals take priority over coordinator generated unbind tasks.
  bool override_existing = opts.driver_host_requested && !active_unbind_->driver_host_requested();
  if (!override_existing) {
    return;
  }
  // There is a potential race condition where a driver calls device_remove() on themselves
  // but the device's unbind hook is about to be called due to a parent being removed.
  // Since it is illegal to call device_remove() twice under the old API,
  // drivers handle this by checking whether their device has already been removed in
  // their unbind hook and hence will never reply to their unbind hook.
  if (state_ == Device::State::kUnbinding) {
    if (unbind_completion_) {
      zx_status_t status = CompleteUnbind(ZX_OK);
      if (status != ZX_OK) {
        LOGF(ERROR, "Cannot complete unbind: %s", zx_status_get_string(status));
      }
    }
  } else {
    // |do_unbind| may not match the stored field in the existing unbind task due to
    // the current device_remove / unbind model.
    // For closest compatibility with the current model, we should prioritize
    // driver_host calls to |ScheduleRemove| over our own scheduled unbind tasks for the children.
    active_unbind_->set_do_unbind(opts.do_unbind);
  }
}

void Device::SendUnbind(UnbindCompletion& completion) {
  if (unbind_completion_) {
    // We already have a pending unbind
    return completion(ZX_ERR_UNAVAILABLE);
  }
  VLOGF(1, "Unbinding device %p '%s'", this, name_.data());
  set_state(Device::State::kUnbinding);
  device_controller()->Unbind().ThenExactlyOnce(
      [dev = fbl::RefPtr(this)](
          fidl::WireUnownedResult<fuchsia_device_manager::DeviceController::Unbind>& result) {
        if (!result.ok()) {
          dev->CompleteUnbind(result.status());
          return;
        }
        auto* response = result.Unwrap();
        LOGF(INFO, "Unbound device %p '%s': %s", dev.get(), dev->name().data(),
             zx_status_get_string(response->is_error() ? response->error_value() : ZX_OK));
        dev->CompleteUnbind();
      });
  unbind_completion_ = std::move(completion);
}

void Device::SendCompleteRemove(RemoveCompletion& completion) {
  if (remove_completion_) {
    // We already have a pending remove.
    return completion(ZX_ERR_UNAVAILABLE);
  }
  VLOGF(1, "Completing removal of device %p '%s'", this, name_.data());
  set_state(Device::State::kUnbinding);
  device_controller()->CompleteRemoval().ThenExactlyOnce(
      [dev = fbl::RefPtr(this)](
          fidl::WireUnownedResult<fuchsia_device_manager::DeviceController::CompleteRemoval>&
              result) {
        if (!result.ok()) {
          if (dev->remove_completion_) {
            dev->remove_completion_(result.status());
          }
          dev->DropRemoveTask();
          return;
        }
        auto* response = result.Unwrap();
        LOGF(INFO, "Remove task completing - Driver host has removed device %p '%s': %s", dev.get(),
             dev->name().data(),
             zx_status_get_string(response->is_error() ? response->error_value() : ZX_OK));
        dev->CompleteRemove();
      });
  remove_completion_ = std::move(completion);
}

zx_status_t Device::CompleteUnbind(zx_status_t status) {
  if (!unbind_completion_ && status == ZX_OK) {
    LOGF(ERROR, "Unexpected reply when unbinding device %p '%s'", this, name_.data());
    return ZX_ERR_IO;
  }
  if (unbind_completion_) {
    unbind_completion_(status);
  }
  DropUnbindTask();
  return ZX_OK;
}

zx_status_t Device::CompleteRemove(zx_status_t status) {
  if (!remove_completion_ && status == ZX_OK) {
    LOGF(ERROR, "Unexpected reply when removing device %p '%s'", this, name_.data());
    return ZX_ERR_IO;
  }
  // If we received an error, it is because we are currently force removing the device.
  if (status == ZX_OK) {
    coordinator->device_manager()->RemoveDevice(fbl::RefPtr(this), false);
  }
  if (remove_completion_) {
    // If we received an error, it is because we are currently force removing the device.
    // In that case, all other devices in the driver_host will be force removed too,
    // and they will call CompleteRemove() before the remove task is scheduled to run.
    // For ancestor dependents in other driver_hosts, we want them to proceed removal as usual.
    remove_completion_(ZX_OK);
  }
  DropRemoveTask();
  return ZX_OK;
}

zx_status_t Device::SetProps(fbl::Array<const zx_device_prop_t> props) {
  // This function should only be called once
  ZX_DEBUG_ASSERT(props_.data() == nullptr);

  props_ = std::move(props);

  return ZX_OK;
}

zx_status_t Device::SetStrProps(fbl::Array<const StrProperty> str_props) {
  // This function should only be called once.
  ZX_DEBUG_ASSERT(!str_props_.data());
  str_props_ = std::move(str_props);

  // Ensure that the string properties are encoded in UTF-8 format.
  for (const auto& str_prop : str_props_) {
    if (!fxl::IsStringUTF8(str_prop.key)) {
      return ZX_ERR_INVALID_ARGS;
    }

    if (str_prop.value.valueless_by_exception()) {
      return ZX_ERR_INVALID_ARGS;
    }

    if (str_prop.value.index() == StrPropValueType::String) {
      const auto str_value = std::get<StrPropValueType::String>(str_prop.value);
      if (!fxl::IsStringUTF8(str_value)) {
        return ZX_ERR_INVALID_ARGS;
      }
    }

    if (str_prop.value.index() == StrPropValueType::Enum) {
      const auto enum_value = std::get<StrPropValueType::Enum>(str_prop.value);
      if (!fxl::IsStringUTF8(enum_value)) {
        return ZX_ERR_INVALID_ARGS;
      }
    }
  }

  return ZX_OK;
}

void Device::set_host(fbl::RefPtr<DriverHost> host) {
  if (host_) {
    host_->devices().erase(*this);
  }
  host_ = std::move(host);
  set_local_id(0);
  if (host_) {
    host_->devices().push_back(this);
    set_local_id(host_->new_device_id());
  }
}

// TODO(fxb/74654): Implement support for string properties.
void Device::AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) {
  auto parent = fbl::RefPtr(this);
  std::string_view name(request->args.name.data(), request->args.name.size());
  std::string_view driver_path(request->args.driver_path.data(), request->args.driver_path.size());

  fbl::RefPtr<Device> device;
  zx_status_t status = parent->coordinator->device_manager()->AddDevice(
      parent, std::move(request->device_controller), std::move(request->coordinator),
      request->args.property_list.props.data(), request->args.property_list.props.count(),
      request->args.property_list.str_props.data(), request->args.property_list.str_props.count(),
      name, request->args.protocol_id, driver_path, request->args.device_add_config,
      request->args.has_init, kEnableAlwaysInit, std::move(request->inspect),
      std::move(request->outgoing_dir), &device);

  if (device != nullptr) {
    device->dfv2_device_symbol_ = request->args.dfv2_device_symbol;
  }

  uint64_t local_id = device != nullptr ? device->local_id() : 0;
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess(local_id);
  }
}

void Device::ScheduleRemove(ScheduleRemoveRequestView request,
                            ScheduleRemoveCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);

  VLOGF(1, "Scheduling remove of device %p '%s'", dev.get(), dev->name().data());

  dev->coordinator->device_manager()->ScheduleDriverHostRequestedRemove(dev, request->unbind_self);
}

void Device::ScheduleUnbindChildren(ScheduleUnbindChildrenCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);

  if (dev->children().empty()) {
    VLOGF(1, "Skipping unbind, no children for device %p '%s'", dev.get(), dev->name().data());
    completer.ReplySuccess(false);
    return;
  }

  VLOGF(1, "Scheduling unbind of children for device %p '%s'", dev.get(), dev->name().data());
  dev->coordinator->device_manager()->ScheduleDriverHostRequestedUnbindChildren(dev);
  completer.ReplySuccess(true);
}

void Device::ConnectFidlProtocol(ConnectFidlProtocolRequestView request,
                                 ConnectFidlProtocolCompleter::Sync& completer) {
  fbl::RefPtr<Device> device = nullptr;
  if (request->fragment_name.is_null()) {
    device = fbl::RefPtr(this);
    if (device->flags & DEV_CTX_PROXY) {
      device = device->parent();
    }
  } else {
    if (!is_composite()) {
      completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
      return;
    }

    for (auto& fragment : composite()->get().fragments()) {
      if (fragment.name() == request->fragment_name.get()) {
        device = fragment.bound_device();
        break;
      }
    }
  }
  if (device == nullptr) {
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }
  if (!device->has_outgoing_directory()) {
    LOGF(ERROR, "`%s` attempted to ConnectFidlProtocol but there is no outgoing directory",
         name().c_str());
    completer.ReplyError(ZX_ERR_BAD_STATE);
    return;
  }

  if (request->service_name.is_null() == false) {
    ZX_ASSERT_MSG(bound_driver_ != nullptr, "Expected device  '%s' to have a driver bound to it",
                  name().c_str());
    auto it = std::find(bound_driver_->service_uses.begin(), bound_driver_->service_uses.end(),
                        std::string("/svc/").append(request->service_name.get()));
    if (it == bound_driver_->service_uses.end()) {
      LOGF(ERROR, "`%s` attempted to use `%s`, but it is not declared in its component manifest",
           name().c_str(), std::string(request->service_name.get()).c_str());
      completer.ReplyError(ZX_ERR_NOT_FOUND);
      return;
    }
  }

  fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
  path.Append("svc/");
  if (request->service_name.is_null() == false) {
    path.Append(request->service_name.get());
    path.Append("/default/");
  }
  path.Append(request->protocol_name.get());
  zx_status_t status = fdio_service_connect_at(device->outgoing_dir_.channel().get(), path.c_str(),
                                               request->server.release());
  completer.Reply(zx::make_result(status));
}

void Device::BindDevice(BindDeviceRequestView request, BindDeviceCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);
  std::string_view driver_url_suffix(request->driver_url_suffix.data(),
                                     request->driver_url_suffix.size());

  if (dev->coordinator->suspend_resume_manager().InSuspend()) {
    LOGF(ERROR, "'bind-device' is forbidden in suspend");
    completer.ReplyError(ZX_ERR_BAD_STATE);
    return;
  }

  VLOGF(1, "'bind-device' device %p '%s'", dev.get(), dev->name().data());
  zx_status_t status =
      dev->coordinator->bind_driver_manager().BindDriverToDevice(dev, driver_url_suffix);

  // Notify observers that this device is available again
  // Needed for non-auto-binding drivers like GPT against block, etc
  if (driver_url_suffix.empty()) {
    devfs.advertise_modified();
  }
  completer.Reply(zx::make_result(status));
}

void Device::GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) {
  std::string path = MakeTopologicalPath();
  completer.ReplySuccess(fidl::StringView::FromExternal(path));
}

void Device::LoadFirmware(LoadFirmwareRequestView request, LoadFirmwareCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);

  std::string driver_path = std::string(request->driver_path.get());
  std::string firmware_path = std::string(request->fw_path.get());

  const Driver* driver = nullptr;
  // Unfortunately we have to walk upwards to look for the driver because clients might be
  // calling load_firmware on a child device that they've created instead of the main device.
  auto walk_dev = fbl::RefPtr(this);
  while (walk_dev) {
    if (walk_dev->bound_driver_ && walk_dev->bound_driver_->url == driver_path) {
      driver = walk_dev->bound_driver_;
      break;
    }
    walk_dev = walk_dev->parent();
  }
  if (!driver) {
    LOGF(ERROR, "Wasn't able to find driver %s for device %s", driver_path.c_str(),
         MakeTopologicalPath().c_str());
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }

  dev->coordinator->firmware_loader().LoadFirmware(
      driver, firmware_path.c_str(),
      [completer = completer.ToAsync()](zx::result<LoadFirmwareResult> result) mutable {
        if (result.is_error()) {
          completer.ReplyError(result.status_value());
          return;
        }
        completer.ReplySuccess(std::move(result->vmo), result->size);
      });
}

void Device::GetMetadata(GetMetadataRequestView request, GetMetadataCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);
  uint8_t data[fuchsia_device_manager::wire::kMetadataBytesMax];
  size_t actual = 0;
  zx_status_t status =
      dev->coordinator->GetMetadata(dev, request->key, data, sizeof(data), &actual);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  auto data_view = ::fidl::VectorView<uint8_t>::FromExternal(data, actual);
  completer.ReplySuccess(data_view);
}

void Device::GetMetadataSize(GetMetadataSizeRequestView request,
                             GetMetadataSizeCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);
  size_t size;
  zx_status_t status = dev->coordinator->GetMetadataSize(dev, request->key, &size);
  if (status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  completer.ReplySuccess(size);
}

void Device::AddMetadata(AddMetadataRequestView request, AddMetadataCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);
  zx_status_t status = dev->coordinator->AddMetadata(dev, request->key, request->data.data(),
                                                     static_cast<uint32_t>(request->data.count()));
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess();
  }
}

void Device::AddCompositeDevice(AddCompositeDeviceRequestView request,
                                AddCompositeDeviceCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);
  std::string_view name(request->name.data(), request->name.size());
  zx_status_t status =
      this->coordinator->device_manager()->AddCompositeDevice(dev, name, request->comp_desc);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess();
  }
}

void Device::AddCompositeNodeSpec(AddCompositeNodeSpecRequestView request,
                                  AddCompositeNodeSpecCompleter::Sync& completer) {
  auto dev = fbl::RefPtr(this);
  zx_status_t status =
      this->coordinator->AddCompositeNodeSpec(dev, request->name.get(), request->spec);
  if (status != ZX_OK) {
    completer.ReplyError(status);
  } else {
    completer.ReplySuccess();
  }
}

bool Device::DriverLivesInSystemStorage() const {
  return StringHasPrefix("/system/", parent_driver_url()) ||
         StringHasPrefix("fuchsia-pkg://", parent_driver_url());
}

bool Device::IsAlreadyBound() const {
  return (flags & DEV_CTX_BOUND) && !(flags & DEV_CTX_ALLOW_MULTI_COMPOSITE);
}

void Device::set_bound_driver(const Driver* driver) {
  ZX_ASSERT_MSG(bound_driver_ == nullptr,
                "Device  '%s' already has driver %s bound to it, cannot bind %s, flags: 0x%x",
                name().c_str(), bound_driver_->url.c_str(), driver->url.c_str(), flags);
  if (!(flags & DEV_CTX_ALLOW_MULTI_COMPOSITE)) {
    flags |= DEV_CTX_BOUND;
    bound_driver_ = driver;
  }
}

void Device::clear_bound_driver() {
  flags &= (~DEV_CTX_BOUND);
  bound_driver_ = nullptr;
}

std::shared_ptr<dfv2::Node> Device::GetBoundNode() {
  if (!dfv2_bound_device_) {
    return nullptr;
  }
  return dfv2_bound_device_->node();
}

zx::result<std::shared_ptr<dfv2::Node>> Device::CreateDFv2Device() {
  if (dfv2_bound_device_) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  std::string full_path = MakeTopologicalPath();
  // The topo_path needs to remove the leading "/dev/".
  auto topo_path = full_path.substr(std::string_view("/dev/").size());

  std::string name = std::to_string(coordinator->GetNextDfv2DeviceId());

  // Create the DeviceServer for the driver.
  std::optional<compat::ServiceOffersV1> service_offers;
  if (has_outgoing_directory()) {
    zx::result outgoing_dir = clone_outgoing_dir();
    if (outgoing_dir.is_error()) {
      return outgoing_dir.take_error();
    }
    // TODO(fxbug.dev/109809): Connect the FIDL offers here.
    service_offers.emplace(name, std::move(outgoing_dir.value()), compat::FidlServiceOffers());
  }
  auto server = compat::DeviceServer(name, protocol_id_, topo_path, std::move(service_offers));

  // Set the metadata for the DeviceServer.
  for (auto& metadata : metadata()) {
    zx_status_t status = server.AddMetadata(metadata.type, metadata.Data(), metadata.length);
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  DeviceInspect inspect = inspect_.CreateChild(name, zx::vmo(), protocol_id_);
  auto dfv2_device = dfv2::Device::CreateAndServe(
      topo_path, name, dfv2_device_symbol_, std::move(inspect), coordinator->dispatcher(),
      &coordinator->outgoing(), std::move(server), this, host().get());
  if (dfv2_device.is_error()) {
    return dfv2_device.take_error();
  }
  dfv2_bound_device_ = std::move(*dfv2_device);
  flags |= DEV_CTX_BOUND;
  DevfsDevice& node_devfs = dfv2_bound_device_->node()->devfs_device();
  // TODO(https://fxbug.dev/125288): Rework the topology to not need this dfv2 devfs node.
  devfs.topological_node()->add_child("dfv2", std::nullopt, Devnode::NoRemote{}, node_devfs);
  node_devfs.publish();

  return zx::ok(dfv2_bound_device_->node());
}

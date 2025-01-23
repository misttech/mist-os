// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "devfs.h"

#include <fidl/fuchsia.device.fs/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/ddk/driver.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/coding.h>
#include <lib/fidl/cpp/message_part.h>
#include <lib/fidl/txn_header.h>
#include <lib/zx/channel.h>
#include <stdio.h>
#include <string.h>
#include <zircon/types.h>

#include <functional>
#include <memory>
#include <random>
#include <unordered_set>

#include <fbl/ref_ptr.h>

#include "src/devices/bin/driver_manager/devfs/builtin_devices.h"
#include "src/devices/bin/driver_manager/devfs/class_names.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace driver_manager {

namespace fio = fuchsia_io;

std::string_view Devnode::name() const {
  if (name_.has_value()) {
    return name_.value();
  }
  return {};
}

void Devnode::advertise_modified() {
  ZX_ASSERT(parent_ != nullptr);
  parent_->Notify(name(), fio::wire::WatchEvent::kRemoved);
  parent_->Notify(name(), fio::wire::WatchEvent::kAdded);
}

Devnode::VnodeImpl::VnodeImpl(Devnode& holder, Target target)
    : holder_(holder), target_(std::move(target)) {}

bool Devnode::VnodeImpl::IsDirectory() const { return !target_.has_value(); }

fuchsia_io::NodeProtocolKinds Devnode::VnodeImpl::GetProtocols() const {
  fuchsia_io::NodeProtocolKinds protocols = fuchsia_io::NodeProtocolKinds::kDirectory;
  if (!IsDirectory()) {
    protocols = protocols | fuchsia_io::NodeProtocolKinds::kConnector;
  }
  return protocols;
}

zx_status_t Devnode::VnodeImpl::ConnectService(zx::channel channel) {
  if (!target_.has_value()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  return (*target_->device_connect.get())(std::move(channel));
}

zx::result<fs::VnodeAttributes> Devnode::VnodeImpl::GetAttributes() const {
  return children().GetAttributes();
}

zx_status_t Devnode::VnodeImpl::Lookup(std::string_view name, fbl::RefPtr<fs::Vnode>* out) {
  return children().Lookup(name, out);
}

zx_status_t Devnode::VnodeImpl::WatchDir(fs::FuchsiaVfs* vfs, fio::wire::WatchMask mask,
                                         uint32_t options,
                                         fidl::ServerEnd<fio::DirectoryWatcher> watcher) {
  return children().WatchDir(vfs, mask, options, std::move(watcher));
}

zx_status_t Devnode::VnodeImpl::Readdir(fs::VdirCookie* cookie, void* dirents, size_t len,
                                        size_t* out_actual) {
  return children().Readdir(cookie, dirents, len, out_actual);
}

namespace {

void MustAddEntry(PseudoDir& parent, const std::string_view name,
                  const fbl::RefPtr<fs::Vnode>& dn) {
  const zx_status_t status = parent.AddEntry(name, dn);
  ZX_ASSERT_MSG(status == ZX_OK, "AddEntry(%.*s): %s", static_cast<int>(name.size()), name.data(),
                zx_status_get_string(status));
}

}  // namespace

Devnode::Devnode(Devfs& devfs)
    : devfs_(devfs), parent_(nullptr), node_(fbl::MakeRefCounted<VnodeImpl>(*this, Target())) {}

Devnode::Devnode(Devfs& devfs, PseudoDir& parent, Target target, fbl::String name)
    : devfs_(devfs),
      parent_(&parent),
      node_(fbl::MakeRefCounted<VnodeImpl>(*this, target)),
      name_([this, &parent, name = std::move(name)]() {
        auto [it, inserted] = parent.unpublished.emplace(name, *this);
        ZX_ASSERT(inserted);
        return it->first;
      }()) {
  if (target.has_value()) {
    children().AddEntry(
        fuchsia_device_fs::wire::kDeviceControllerName,
        fbl::MakeRefCounted<fs::Service>([passthrough = target](zx::channel channel) {
          return (*passthrough->controller_connect.get())(
              fidl::ServerEnd<fuchsia_device::Controller>(std::move(channel)));
        }));
    children().AddEntry(
        fuchsia_device_fs::wire::kDeviceProtocolName,
        fbl::MakeRefCounted<fs::Service>([passthrough = target](zx::channel channel) {
          return (*passthrough->device_connect.get())(std::move(channel));
        }));
  }
}

std::optional<std::reference_wrapper<fs::Vnode>> Devfs::Lookup(PseudoDir& parent,
                                                               std::string_view name) {
  {
    fbl::RefPtr<fs::Vnode> out;
    switch (const zx_status_t status = parent.Lookup(name, &out); status) {
      case ZX_OK:
        return *out;
      case ZX_ERR_NOT_FOUND:
        break;
      default:
        ZX_PANIC("%s", zx_status_get_string(status));
    }
  }
  const auto it = parent.unpublished.find(name);
  if (it != parent.unpublished.end()) {
    return it->second.get().node();
  }
  return {};
}

Devnode::~Devnode() {
  for (auto [key, child] : children().unpublished) {
    child.get().parent_ = nullptr;
  }
  children().unpublished.clear();

  children().RemoveAllEntries();

  if (parent_ == nullptr) {
    return;
  }
  PseudoDir& parent = *parent_;
  const std::string_view name = this->name();
  parent.unpublished.erase(name);
  switch (const zx_status_t status = parent.RemoveEntry(name, node_.get()); status) {
    case ZX_OK:
    case ZX_ERR_NOT_FOUND:
      // Our parent may have been removed before us.
      break;
    default:
      ZX_PANIC("RemoveEntry(%.*s): %s", static_cast<int>(name.size()), name.data(),
               zx_status_get_string(status));
  }
}

void Devnode::publish() {
  ZX_ASSERT(parent_ != nullptr);
  PseudoDir& parent = *parent_;

  const std::string_view name = this->name();
  const auto it = parent.unpublished.find(name);
  ZX_ASSERT(it != parent.unpublished.end());
  ZX_ASSERT(&it->second.get() == this);
  parent.unpublished.erase(it);

  MustAddEntry(parent, name, node_);
}

void DevfsDevice::advertise_modified() {
  if (topological_.has_value()) {
    topological_.value().advertise_modified();
  }
  if (protocol_.has_value()) {
    protocol_.value().advertise_modified();
  }
}

void DevfsDevice::publish() {
  if (topological_.has_value()) {
    topological_.value().publish();
  }
  if (protocol_.has_value()) {
    protocol_.value().publish();
  }
}

void DevfsDevice::unpublish() {
  topological_.reset();
  protocol_.reset();
}

zx::result<std::string> Devfs::MakeInstanceName(std::string_view class_name) {
  if (!class_entries_.contains(std::string(class_name))) {
    class_entries_[std::string(class_name)] = fbl::MakeRefCounted<PseudoDir>();
    zx_status_t status = class_->AddEntry(class_name, class_entries_[std::string(class_name)]);
    if (status != ZX_OK) {
      LOGF(WARNING, "Failed to add class name '%.*s'  %s", static_cast<int>(class_name.size()),
           class_name.data(), zx_status_get_string(status));
      class_entries_.erase(std::string(class_name));
      return zx::error(status);
    }
  }
  if (classes_that_assume_ordering.contains(std::string(class_name))) {
    // must give a sequential id:
    return zx::ok(std::format("{:03d}", classes_that_assume_ordering[class_name]++));
  }
  std::uniform_int_distribution<uint32_t> distrib(0, 0xffffffff);
  return zx::ok(std::format("{}", distrib(device_number_generator_)));
}

zx_status_t Devnode::add_child(std::string_view name, std::optional<std::string_view> class_name,
                               Target target, DevfsDevice& out_child) {
  // Check that the child does not have a duplicate name.
  const std::optional other = devfs_.Lookup(children(), name);
  if (other.has_value()) {
    LOGF(WARNING, "rejecting duplicate device name '%.*s'", static_cast<int>(name.size()),
         name.data());
    return ZX_ERR_ALREADY_EXISTS;
  }

  // Export the device to its class directory.
  if (class_name.has_value()) {
    zx::result<std::string> instance_name = devfs_.MakeInstanceName(class_name.value());
    if (instance_name.is_ok()) {
      out_child.protocol_node().emplace(devfs_, *devfs_.get_class_entry(class_name.value()), target,
                                        instance_name.value());
    }
  }

  out_child.topological_node().emplace(devfs_, children(), std::move(target), name);

  return ZX_OK;
}

zx::result<fidl::ClientEnd<fio::Directory>> Devfs::Connect(fs::FuchsiaVfs& vfs) {
  auto [client, server] = fidl::Endpoints<fio::Directory>::Create();
  // NB: Serve the `PseudoDir` rather than the root `Devnode` because
  // otherwise we'd end up in the connector code path. Clients that want to open
  // the root node as a device can do so using `"."` and appropriate flags.
  return zx::make_result(vfs.ServeDirectory(root_.node_, std::move(server)), std::move(client));
}

Devfs::Devfs(std::optional<Devnode>& root) : root_(root.emplace(*this)) {
  PseudoDir& pd = root_.children();
  MustAddEntry(pd, "class", class_);
  MustAddEntry(pd, kNullDevName, fbl::MakeRefCounted<BuiltinDevVnode>(true));
  MustAddEntry(pd, kZeroDevName, fbl::MakeRefCounted<BuiltinDevVnode>(false));
  {
    fbl::RefPtr builtin = fbl::MakeRefCounted<PseudoDir>();
    MustAddEntry(*builtin, kNullDevName, fbl::MakeRefCounted<BuiltinDevVnode>(true));
    MustAddEntry(*builtin, kZeroDevName, fbl::MakeRefCounted<BuiltinDevVnode>(false));
    MustAddEntry(pd, "builtin", std::move(builtin));
  }
  for (std::string class_name : kEagerClassNames) {
    EnsureClassExists(class_name);
  }
}

zx_status_t Devnode::export_class(Devnode::Target target, std::string_view class_name,
                                  std::vector<std::unique_ptr<Devnode>>& out) {
  zx::result<std::string> instance_name = devfs_.MakeInstanceName(class_name);
  if (instance_name.is_error()) {
    return instance_name.status_value();
  }
  Devnode& child = *out.emplace_back(std::make_unique<Devnode>(
      devfs_, *devfs_.get_class_entry(class_name), target, instance_name.value()));

  child.publish();
  return ZX_OK;
}

void Devfs::EnsureClassExists(std::string_view name) {
  if (class_entries_.contains(std::string(name))) {
    return;
  }
  class_entries_[std::string(name)] = fbl::MakeRefCounted<PseudoDir>();
  MustAddEntry(*class_, name, class_entries_[std::string(name)]);
}

zx_status_t Devnode::export_topological_path(Devnode::Target target,
                                             std::string_view topological_path,
                                             std::vector<std::unique_ptr<Devnode>>& out) {
  // Validate the topological path.
  const std::vector segments =
      fxl::SplitString(topological_path, "/", fxl::WhiteSpaceHandling::kKeepWhitespace,
                       fxl::SplitResult::kSplitWantAll);
  if (segments.empty() ||
      std::any_of(segments.begin(), segments.end(), std::mem_fn(&std::string_view::empty))) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Walk the request export path segment-by-segment.
  Devnode* dn = this;
  for (size_t i = 0; i < segments.size(); ++i) {
    const std::string_view name = segments.at(i);
    zx::result child = [name, &children = dn->children()]() -> zx::result<Devnode*> {
      fbl::RefPtr<fs::Vnode> out;
      switch (const zx_status_t status = children.Lookup(name, &out); status) {
        case ZX_OK:
          return zx::ok(&fbl::RefPtr<Devnode::VnodeImpl>::Downcast(out)->holder_);
        case ZX_ERR_NOT_FOUND:
          break;
        default:
          return zx::error(status);
      }
      const auto it = children.unpublished.find(name);
      if (it != children.unpublished.end()) {
        return zx::ok(&it->second.get());
      }
      return zx::ok(nullptr);
    }();
    if (child.is_error()) {
      return child.status_value();
    }
    if (i != segments.size() - 1) {
      // This is not the final path segment. Use the existing node or create one
      // if it doesn't exist.
      if (child.value() != nullptr) {
        dn = child.value();
        continue;
      }
      PseudoDir& parent = dn->node().children();
      Devnode& child = *out.emplace_back(std::make_unique<Devnode>(devfs_, parent, Target{}, name));
      child.publish();
      dn = &child;
      continue;
    }

    // At this point `dn` is the second-last path segment.
    if (child != nullptr) {
      // The full path described by `devfs_path` already exists.
      return ZX_ERR_ALREADY_EXISTS;
    }

    // Create the final child.
    {
      Devnode& child = *out.emplace_back(
          std::make_unique<Devnode>(devfs_, dn->node().children(), std::move(target), name));
      child.publish();
    }
  }
  return ZX_OK;
}

zx_status_t Devnode::export_dir(Devnode::Target target,
                                std::optional<std::string_view> topological_path,
                                std::optional<std::string_view> class_path,
                                std::vector<std::unique_ptr<Devnode>>& out) {
  if (topological_path.has_value()) {
    zx_status_t status = export_topological_path(target, topological_path.value(), out);
    if (status != ZX_OK) {
      return status;
    }
  }

  if (class_path.has_value()) {
    zx_status_t status = export_class(target, class_path.value(), out);
    if (status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
}
}  // namespace driver_manager

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
#include "src/devices/lib/log/log.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/storage/lib/vfs/cpp/fuchsia_vfs.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace driver_manager {
namespace {

struct ProtocolInfo {
  std::string_view name;
  uint32_t id;
  uint32_t flags;
};

constexpr ProtocolInfo proto_infos[] = {
#define DDK_PROTOCOL_DEF(tag, val, name, flags) {name, val, flags},
#include <lib/ddk/protodefs.h>
};

Devnode::Target clone_target(Devnode::Target& target) { return target; }

}  // namespace

namespace fio = fuchsia_io;

std::optional<std::string_view> ProtocolIdToClassName(uint32_t protocol_id) {
  for (const ProtocolInfo& info : proto_infos) {
    if (info.id != protocol_id) {
      continue;
    }
    if (info.flags & PF_NOPUB) {
      return std::nullopt;
    }
    return info.name;
  }
  return std::nullopt;
}

std::optional<std::reference_wrapper<ProtoNode>> Devfs::proto_node(std::string_view protocol_name) {
  for (const ProtocolInfo& info : proto_infos) {
    if (info.name == protocol_name) {
      return proto_node(info.id);
    }
  }
  return std::nullopt;
}

std::optional<std::reference_wrapper<ProtoNode>> Devfs::proto_node(uint32_t protocol_id) {
  auto it = proto_info_nodes.find(protocol_id);
  if (it == proto_info_nodes.end()) {
    return std::nullopt;
  }
  auto& [key, value] = *it;
  return *value;
}

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
      node_(fbl::MakeRefCounted<VnodeImpl>(*this, clone_target(target))),
      name_([this, &parent, name = std::move(name)]() {
        auto [it, inserted] = parent.unpublished.emplace(name, *this);
        ZX_ASSERT(inserted);
        return it->first;
      }()) {
  if (target.has_value()) {
    children().AddEntry(
        fuchsia_device_fs::wire::kDeviceControllerName,
        fbl::MakeRefCounted<fs::Service>([passthrough = target->Clone()](zx::channel channel) {
          return (*passthrough.controller_connect.get())(
              fidl::ServerEnd<fuchsia_device::Controller>(std::move(channel)));
        }));
    children().AddEntry(
        fuchsia_device_fs::wire::kDeviceProtocolName,
        fbl::MakeRefCounted<fs::Service>([passthrough = target->Clone()](zx::channel channel) {
          return (*passthrough.device_connect.get())(std::move(channel));
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

ProtoNode::ProtoNode(fbl::String name) : name_(std::move(name)) {}

SequentialProtoNode::SequentialProtoNode(fbl::String name) : ProtoNode(std::move(name)) {}

uint32_t SequentialProtoNode::allocate_device_number() {
  return (next_device_number_++) % (maximum_device_number_ + 1);
}

const char* SequentialProtoNode::format() { return format_; }

RandomizedProtoNode::RandomizedProtoNode(fbl::String name,
                                         std::default_random_engine::result_type seed)
    : ProtoNode(std::move(name)), device_number_generator_(seed) {}

uint32_t RandomizedProtoNode::allocate_device_number() {
  std::uniform_int_distribution<uint32_t> distrib(0, maximum_device_number_);
  return distrib(device_number_generator_);
}

const char* RandomizedProtoNode::format() { return format_; }

zx::result<fbl::String> ProtoNode::seq_name() {
  std::string dest;
  for (uint32_t i = 0; i < 1000; ++i) {
    dest.clear();
    fxl::StringAppendf(&dest, format(), allocate_device_number());
    {
      fbl::RefPtr<fs::Vnode> out;
      switch (const zx_status_t status = children().Lookup(dest, &out); status) {
        case ZX_OK:
          continue;
        case ZX_ERR_NOT_FOUND:
          break;
        default:
          return zx::error(status);
      }
    }
    if (children().unpublished.find(dest) != children().unpublished.end()) {
      continue;
    }
    return zx::ok(dest);
  }
  return zx::error(ZX_ERR_ALREADY_EXISTS);
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
    std::optional proto_dir = devfs_.proto_node(class_name.value());
    if (proto_dir.has_value()) {
      zx::result seq_name = proto_dir.value().get().seq_name();
      if (seq_name.is_error()) {
        return seq_name.status_value();
      }
      fbl::String instance_name = seq_name.value();

      Devnode::Target target_clone = clone_target(target);
      out_child.protocol_node().emplace(devfs_, proto_dir.value().get().children(),
                                        std::move(target_clone), instance_name);
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

  // TODO(https://fxbug.dev/42064970): shrink this list to zero.
  //
  // Do not add to this list.
  //
  // These classes have clients that rely on the numbering scheme starting at
  // 000 and increasing sequentially. This list was generated using:
  //
  // rg -IoN --no-ignore -g '!out/' -g '!*.md' '\bclass/[^/]+/[0-9]{3}\b' | \
  // sed -E 's|class/(.*)/[0-9]{3}|"\1",|g' | sort | uniq
  const std::unordered_set<std::string_view> classes_that_assume_ordering({
      // TODO(https://fxbug.dev/42065012): Remove.
      "adc",

      // TODO(https://fxbug.dev/42065013): Remove.
      "aml-ram",

      // TODO(https://fxbug.dev/42065014): Remove.
      // TODO(https://fxbug.dev/42065080): Remove.
      "backlight",

      // TODO(https://fxbug.dev/42068339): Remove.
      "block",

      // TODO(https://fxbug.dev/42065067): Remove.
      "goldfish-address-space",
      "goldfish-control",
      "goldfish-pipe",

      // TODO(https://fxbug.dev/42065072): Remove.
      "ot-radio",

      // TODO(https://fxbug.dev/42065076): Remove.
      "securemem",

      // TODO(https://fxbug.dev/42065009): Remove.
      // TODO(https://fxbug.dev/42065080): Remove.
      "temperature",

      // TODO(https://fxbug.dev/42065080): Remove.
      "thermal",
  });
  // Pre-populate the class directories.
  std::random_device rd;
  for (const auto& info : proto_infos) {
    if (!(info.flags & PF_NOPUB)) {
      std::unique_ptr<ProtoNode>& value = proto_info_nodes[info.id];
      ZX_ASSERT_MSG(value == nullptr, "duplicate protocol with id %d", info.id);
      if (classes_that_assume_ordering.find(info.name) != classes_that_assume_ordering.end()) {
        value = std::make_unique<SequentialProtoNode>(info.name);
      } else {
        value = std::make_unique<RandomizedProtoNode>(info.name, rd());
      }
      MustAddEntry(*class_, info.name, value->children_);
    }
  }
}

zx_status_t Devnode::export_class(Devnode::Target target, std::string_view class_path,
                                  std::vector<std::unique_ptr<Devnode>>& out) {
  std::optional proto_node = devfs_.proto_node(class_path);
  if (!proto_node.has_value()) {
    return ZX_ERR_NOT_FOUND;
  }

  ProtoNode& dn = proto_node.value().get();
  zx::result seq_name = dn.seq_name();
  if (seq_name.is_error()) {
    return seq_name.error_value();
  }
  const fbl::String name = seq_name.value();

  Devnode& child =
      *out.emplace_back(std::make_unique<Devnode>(devfs_, dn.children(), std::move(target), name));
  child.publish();
  return ZX_OK;
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
    Devnode::Target target_clone = clone_target(target);
    zx_status_t status =
        export_topological_path(std::move(target_clone), topological_path.value(), out);
    if (status != ZX_OK) {
      return status;
    }
  }

  if (class_path.has_value()) {
    zx_status_t status = export_class(std::move(target), class_path.value(), out);
    if (status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
}
}  // namespace driver_manager

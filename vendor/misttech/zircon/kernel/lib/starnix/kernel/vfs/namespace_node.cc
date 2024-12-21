// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/namespace_node.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/file_ops.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/kernel/vfs/mount.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <trace.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

bool NamespaceNode::operator==(const NamespaceNode& other) const {
  return (mount_.handle_ == other.mount_.handle_) && (entry_ == other.entry_);
}

NamespaceNode NamespaceNode::New(MountHandle mount, DirEntryHandle dir_entry) {
  return NamespaceNode{.mount_ = MountInfo{.handle_ = mount}, .entry_ = ktl::move(dir_entry)};
}

/// Create a namespace node that is not mounted in a namespace.
NamespaceNode NamespaceNode::new_anonymous(DirEntryHandle dir_entry) {
  return NamespaceNode{.mount_ = {}, .entry_ = ktl::move(dir_entry)};
}

/// Create a namespace node that is not mounted in a namespace and that refers to a node that
/// is not rooted in a hierarchy and has no name.
NamespaceNode NamespaceNode::new_anonymous_unrooted(FsNodeHandle node) {
  return new_anonymous(DirEntry::new_unrooted(ktl::move(node)));
}

fit::result<Errno, FileHandle> NamespaceNode::open(const CurrentTask& current_task, OpenFlags flags,
                                                   AccessCheck access_check) const {
  auto open = entry_->node_->open(current_task, mount_, flags, access_check) _EP(open);
  return FileObject::New(ktl::move(open.value()), *this, flags);
}

fit::result<Errno, NamespaceNode> NamespaceNode::open_create_node(const CurrentTask& current_task,
                                                                  const FsStr& name, FileMode mode,
                                                                  DeviceType dev,
                                                                  OpenFlags flags) const {
  LTRACEF_LEVEL(2, "name=[%.*s],  mode=0x%x\n", static_cast<int>(name.length()), name.data(),
                mode.bits());

  auto owner = current_task->as_fscred();
  auto mask_mode = current_task->fs()->apply_umask(mode);

  auto create_fn = [&current_task, mask_mode, dev, owner](
                       const FsNodeHandle& dir, const MountInfo& mount,
                       const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
    return dir->mknod(current_task, mount, name, mask_mode, dev, owner);
  };

  auto entry_result = [&]() -> fit::result<Errno, DirEntryHandle> {
    if (flags.contains(OpenFlagsEnum::EXCL)) {
      return entry_->create_entry(current_task, mount_, name, create_fn);
    }
    return entry_->get_or_create_entry(current_task, mount_, name, create_fn);
  }() _EP(entry_result);

  return fit::ok(NamespaceNode::with_new_entry(entry_result.value()));
}

ActiveNamespaceNode NamespaceNode::into_active() const { return ActiveNamespaceNode::New(*this); }

fit::result<Errno, NamespaceNode> NamespaceNode::create_node(const CurrentTask& current_task,
                                                             const FsStr& name, FileMode mode,
                                                             DeviceType dev) const {
  LTRACEF_LEVEL(2, "name=[%.*s],  mode=0x%x\n", static_cast<int>(name.length()), name.data(),
                mode.bits());

  auto owner = current_task->as_fscred();
  auto mask_mode = current_task->fs()->apply_umask(mode);
  auto result =
      entry_->create_entry(current_task, mount_, name,
                           [&current_task, mask_mode, dev, owner](
                               const FsNodeHandle& dir, const MountInfo& mount,
                               const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
                             return dir->mknod(current_task, mount, name, mask_mode, dev, owner);
                           }) _EP(result);

  return fit::ok(NamespaceNode::with_new_entry(result.value()));
}

fit::result<Errno, NamespaceNode> NamespaceNode::create_symlink(const CurrentTask& current_task,
                                                                const FsStr& name,
                                                                const FsStr& target) const {
  LTRACEF_LEVEL(2, "name=[%.*s], target=[%.*s]\n", static_cast<int>(name.length()), name.data(),
                static_cast<int>(target.length()), target.data());

  auto owner = current_task->as_fscred();
  auto result = entry_->create_entry(
      current_task, mount_, name,
      [&current_task, &target, owner](const FsNodeHandle& dir, const MountInfo& mount,
                                      const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
        return dir->create_symlink(current_task, mount, name, target, owner);
      }) _EP(result);

  return fit::ok(NamespaceNode::with_new_entry(result.value()));
}

fit::result<Errno, NamespaceNode> NamespaceNode::create_tmpfile(const CurrentTask& current_task,
                                                                FileMode mode,
                                                                OpenFlags flags) const {
  // auto owner = current_task->as_fscred();
  // auto _mode = current_task->fs()->apply_umask(mode);
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno, NamespaceNode> NamespaceNode::link(const CurrentTask& current_task,
                                                      const FsStr& name,
                                                      const FsNodeHandle& child) const {
  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());

  auto result = entry_->create_entry(
      current_task, mount_, name,
      [&current_task, &child](const FsNodeHandle& dir, const MountInfo& mount,
                              const FsStr& name) -> fit::result<Errno, FsNodeHandle> {
        return dir->link(current_task, mount, name, child);
      }) _EP(result);

  return fit::ok(NamespaceNode::with_new_entry(result.value()));
}

fit::result<Errno> NamespaceNode::unlink(const CurrentTask& current_task, const FsStr& name,
                                         UnlinkKind kind, bool must_be_directory) const {
  LTRACEF_LEVEL(2, "name=[%.*s]\n", static_cast<int>(name.length()), name.data());

  if (DirEntry::is_reserved_name(name)) {
    switch (kind) {
      case UnlinkKind::Directory:
        if (name == "..") {
          return fit::error(errno(ENOTEMPTY));
        } else if (!parent().has_value()) {
          // The client is attempting to remove the root.
          return fit::error(errno(EBUSY));
        } else {
          return fit::error(errno(EINVAL));
        }
      case UnlinkKind::NonDirectory:
        return fit::error(errno(ENOTDIR));
    }
  }
  return entry_->unlink(current_task, mount_, name, kind, must_be_directory);
}

fit::result<Errno, NamespaceNode> NamespaceNode::lookup_child(const CurrentTask& current_task,
                                                              LookupContext& context,
                                                              const FsStr& basename) const {
  LTRACEF_LEVEL(2, "%.*s [%.*s]\n", static_cast<int>(debug().length()), debug().data(),
                static_cast<int>(basename.length()), basename.data());

  if (!entry_->node_->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }

  if (basename.size() > static_cast<size_t>(NAME_MAX)) {
    return fit::error(errno(ENAMETOOLONG));
  }

  auto child_result = [&]() -> fit::result<Errno, NamespaceNode> {
    if (basename.empty() || basename == ".") {
      return fit::ok(*this);
    }

    if (basename == "..") {
      auto root =
          ktl::visit(ResolveBase::overloaded{
                         [&](const ktl::monostate&) -> fit::result<Errno, NamespaceNode> {
                           return fit::ok(current_task->fs()->root());
                         },
                         [&](const BeneathTag& beneath) -> fit::result<Errno, NamespaceNode> {
                           // Do not allow traversal out of the 'node'.
                           if (*this == beneath.node) {
                             return fit::error(errno(EXDEV));
                           }
                           return fit::ok(current_task->fs()->root());
                         },
                         [&](const InRootTag& in_root) -> fit::result<Errno, NamespaceNode> {
                           return fit::ok(in_root.node);
                         },
                     },
                     context.resolve_base.variant_) _EP(root);

      // Make sure this can't escape a chroot.
      if (*this == root.value()) {
        return fit::ok(root.value());
      }
      return fit::ok(parent().value_or(*this));
    }

    auto lookup_result =
        entry_->component_lookup(current_task, mount_, basename) _EP(lookup_result);

    auto child = with_new_entry(lookup_result.value());
    while (child.entry_->node_->is_lnk()) {
      bool escape_while = false;
      switch (context.symlink_mode) {
        case SymlinkMode::NoFollow:
          escape_while = true;
          break;
        case SymlinkMode::Follow: {
          if ((context.remaining_follows == 0) ||
              context.resolve_flags.contains(ResolveFlagsEnum::NO_SYMLINKS)) {
            return fit::error(errno(ELOOP));
          }
          context.remaining_follows -= 1;
          auto readlink_result = child.readlink(current_task) _EP(readlink_result);
          auto node = ktl::visit(
              SymlinkTarget::overloaded{
                  [&](const FsString& link_target) -> fit::result<Errno, NamespaceNode> {
                    if (link_target.data()[0] == '/') {
                      auto link_directory = ktl::visit(
                          ResolveBase::overloaded{
                              [&](const ktl::monostate&) -> fit::result<Errno, NamespaceNode> {
                                return fit::ok(current_task->fs()->root());
                              },
                              [&](const BeneathTag&) -> fit::result<Errno, NamespaceNode> {
                                return fit::error(errno(ELOOP));
                              },
                              [&](const InRootTag& in_root) -> fit::result<Errno, NamespaceNode> {
                                return fit::ok(in_root.node);
                              },
                          },
                          context.resolve_base.variant_) _EP(link_directory);
                      return current_task.lookup_path(context, link_directory.value(),
                                                      link_target.data());
                    }
                    return fit::ok(*this);
                  },
                  [&](const NamespaceNode& node) -> fit::result<Errno, NamespaceNode> {
                    if (context.resolve_flags.contains(ResolveFlagsEnum::NO_MAGICLINKS)) {
                      return fit::error(errno(ELOOP));
                    }
                    return fit::ok(node);
                  },
              },
              readlink_result->variant_) _EP(node);

          child = node.value();
        }
      };
      if (escape_while) {
        break;
      }
    }
    return fit::ok(child.enter_mount());
  }() _EP(child_result);

  auto child = child_result.value();
  if (context.resolve_flags.contains(ResolveFlagsEnum::NO_XDEV) &&
      child.mount_.handle_ != mount_.handle_) {
    return fit::error(errno(EXDEV));
  }

  if (context.must_be_directory && !child.entry_->node_->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }

  return fit::ok(child);
}

/// Traverse up a child-to-parent link in the namespace.
///
/// This traversal matches the child-to-parent link in the underlying
/// FsNode except at mountpoints, where the link switches from one
/// filesystem to another.
ktl::optional<NamespaceNode> NamespaceNode::parent() const {
  auto mountpoint_or_self = escape_mount();
  auto parent_entry = mountpoint_or_self.entry_->parent();
  if (!parent_entry.has_value()) {
    return ktl::nullopt;
  }
  return ktl::optional<NamespaceNode>(mountpoint_or_self.with_new_entry(parent_entry.value()));
}

/// Returns the parent, but does not escape mounts i.e. returns None if this node
/// is the root of a mount.
ktl::optional<DirEntryHandle> NamespaceNode::parent_within_mount() const {
  if (mount_if_root().is_ok()) {
    return ktl::nullopt;
  }
  return entry_->parent();
}

NamespaceNode NamespaceNode::with_new_entry(DirEntryHandle entry) const {
  return NamespaceNode{.mount_ = mount_, .entry_ = ktl::move(entry)};
}

fit::result<Errno, UserAndOrGroupId> NamespaceNode::suid_and_sgid(
    const CurrentTask& current_task) const {
  if (mount_.flags().contains(MountFlagsEnum::NOSUID)) {
    return fit::ok(UserAndOrGroupId());
  }
  auto creds = current_task->creds();
  return entry_->node_->info()->suid_and_sgid(creds);
}

NamespaceNode NamespaceNode::enter_mount() const {
  // While the child is a mountpoint, replace child with the mount's root.
  auto enter_one_mount = [](const NamespaceNode& node) -> ktl::optional<NamespaceNode> {
    if (auto& mount_opt = node.mount_.handle_; mount_opt.has_value()) {
      auto state = mount_opt.value()->state_.Read();
      Submount key(node.entry_, fbl::RefPtr<Mount>());
      auto submount = state->submounts_.find(key);
      if (submount != state->submounts_.end()) {
        return (*submount)->mount()->root();
      }
    }
    return {};
  };

  auto inner = *this;
  while (auto inner_root = enter_one_mount(inner)) {
    inner = inner_root.value();
  }
  return inner;
}

NamespaceNode NamespaceNode::escape_mount() const {
  auto mountpoint_or_self = *this;
  while (mountpoint_or_self.mountpoint().has_value()) {
    mountpoint_or_self = mountpoint_or_self.mountpoint().value();
  }
  return mountpoint_or_self;
}

fit::result<Errno, MountHandle> NamespaceNode::mount_if_root() const {
  if (auto mount = (*this->mount_); mount.has_value()) {
    if (entry_ == (*mount)->root_) {
      return fit::ok(mount.value());
    }
  }
  return fit::error(errno(EINVAL));
}

ktl::optional<NamespaceNode> NamespaceNode::mountpoint() const {
  if (auto mount = mount_if_root(); mount.is_ok()) {
    return mount->mountpoint();
  }
  return ktl::nullopt;
}

FsString NamespaceNode::path(const Task& task) const {
  return path_from_root(task.fs()->root()).into_path();
}

FsString NamespaceNode::path_escaping_chroot() const { return path_from_root({}).into_path(); }

PathWithReachability NamespaceNode::path_from_root(ktl::optional<NamespaceNode> root) const {
  if (!this->mount_.handle_.has_value()) {
    return PathWithReachability::Reachable(entry_->local_name());
  }

  auto path = PathBuilder::New();
  auto current = escape_mount();
  if (root.has_value()) {
    // The current node is expected to intersect with the custom root as we travel up the tree.
    auto local_root = root->escape_mount();
    while (current != local_root) {
      if (auto parent = current.parent(); parent.has_value()) {
        path.prepend_element(current.entry_->local_name());
        current = parent->escape_mount();
      } else {
        // This node hasn't intersected with the custom root and has reached the namespace root.
        return PathWithReachability::Unreachable(path.build_absolute());
      }
    }
  } else {
    // No custom root, so travel up the tree to the namespace root.
    while (current.parent().has_value()) {
      path.prepend_element(current.entry_->local_name());
      current = current.parent()->escape_mount();
    }
  }

  return PathWithReachability::Reachable(path.build_absolute());
}

fit::result<Errno> NamespaceNode::mount(WhatToMount what, MountFlags flags) const {
  auto mount_flags =
      flags & (MountFlags(MountFlagsEnum::STORED_ON_MOUNT) | MountFlags(MountFlagsEnum::REC));
  auto mountpoint = enter_mount();
  auto mount = (*mountpoint.mount_).value();
  ZX_ASSERT_MSG(mount, "a mountpoint must be part of a mount");
  mount->create_submount(mountpoint.entry_, what, mount_flags);
  return fit::ok();
}

/// If this is the root of a filesystem, unmount. Otherwise return EINVAL.
fit::result<Errno> NamespaceNode::unmount(UnmountFlags flags) const {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno> NamespaceNode::rename(const CurrentTask& current_task,
                                         const NamespaceNode& old_parent, const FsStr& old_name,
                                         const NamespaceNode& new_parent, const FsStr& new_name,
                                         RenameFlags flags) {
  return DirEntry::rename(current_task, old_parent.entry_, old_parent.mount_, old_name,
                          new_parent.entry_, new_parent.mount_, new_name, flags);
}

fit::result<Errno, SymlinkTarget> NamespaceNode::readlink(const CurrentTask& current_task) const {
  return entry_->node_->readlink(current_task);
}

fit::result<Errno> NamespaceNode::check_access(const CurrentTask& current_task, Access access,
                                               CheckAccessReason reason) const {
  return entry_->node_->check_access(current_task, mount_, access, reason);
}

fit::result<Errno> NamespaceNode::truncate(const CurrentTask& current_task, uint64_t length) const {
  return fit::error(errno(ENOTSUP));
}

mtl::BString NamespaceNode::debug() const {
  auto path = path_escaping_chroot();
  auto mount_fmt = mount_.handle_ ? (*mount_.handle_)->debug() : "{}";
  auto entry_fmt = entry_->debug();
  return mtl::format("NamespaceNode {path=%.*s, mount=%.*s, entry=%.*s}",
                     static_cast<int>(path.size()), path.data(), static_cast<int>(mount_fmt.size()),
                     mount_fmt.data(), static_cast<int>(entry_fmt.size()), entry_fmt.data());
}

// NamespaceNode::NamespaceNode(const NamespaceNode& other) = default;

NamespaceNode& NamespaceNode::operator=(const NamespaceNode& other) = default;

NamespaceNode::~NamespaceNode() { LTRACE_ENTRY_OBJ; }

ActiveNamespaceNode ActiveNamespaceNode::New(NamespaceNode name) {
  ktl::optional<MountClientMarker> marker =
      name.mount_.handle_.has_value()
          ? ktl::optional<MountClientMarker>{(*name.mount_.handle_)->active_client_counter_}
          : ktl::nullopt;
  return ActiveNamespaceNode(name, ktl::move(marker));
}

NamespaceNode ActiveNamespaceNode::to_passive() const {
  auto clone = name_;
  return clone;
}

ActiveNamespaceNode::ActiveNamespaceNode(NamespaceNode name,
                                         ktl::optional<MountClientMarker> marker)
    : name_(name), marker_(ktl::move(marker)) {}

ActiveNamespaceNode::~ActiveNamespaceNode() = default;

const NamespaceNode& ActiveNamespaceNode::operator*() const { return name_; }
const NamespaceNode* ActiveNamespaceNode::operator->() const { return &name_; }
NamespaceNode& ActiveNamespaceNode::operator*() { return name_; }
NamespaceNode* ActiveNamespaceNode::operator->() { return &name_; }

bool ActiveNamespaceNode::operator==(const ActiveNamespaceNode& other) const {
  return name_ == other.name_;
}

SymlinkTarget::SymlinkTarget(Variant variant) : variant_(ktl::move(variant)) {}

SymlinkTarget SymlinkTarget::Path(const FsString& path) { return SymlinkTarget(path); }

SymlinkTarget SymlinkTarget::Node(NamespaceNode node) { return SymlinkTarget(node); }

SymlinkTarget::~SymlinkTarget() = default;

}  // namespace starnix

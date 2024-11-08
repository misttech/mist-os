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
  return NamespaceNode{.mount_ = MountInfo{mount}, .entry_ = ktl::move(dir_entry)};
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
                                                   bool check_access) const {
  auto open = entry_->node_->open(current_task, mount_, flags, check_access) _EP(open);
  return FileObject::New(ktl::move(open.value()), *this, flags);
}

fit::result<Errno, NamespaceNode> NamespaceNode::open_create_node(const CurrentTask& current_task,
                                                                  const FsStr& name, FileMode mode,
                                                                  DeviceType dev, OpenFlags flags) {
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

fit::result<Errno, NamespaceNode> NamespaceNode::create_node(const CurrentTask& current_task,
                                                             const FsStr& name, FileMode mode,
                                                             DeviceType dev) {
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

fit::result<Errno, NamespaceNode> NamespaceNode::create_tmpfile(const CurrentTask& current_task,
                                                                FileMode mode,
                                                                OpenFlags flags) const {
  // auto owner = current_task->as_fscred();
  // auto _mode = current_task->fs()->apply_umask(mode);
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno, NamespaceNode> NamespaceNode::lookup_child(const CurrentTask& current_task,
                                                              LookupContext& context,
                                                              const FsStr& basename) const {
  LTRACEF_LEVEL(2, "basename=[%.*s]\n", static_cast<int>(basename.length()), basename.data());

  if (!entry_->node_->is_dir()) {
    return fit::error(errno(ENOTDIR));
  }

  if (basename.size() > static_cast<size_t>(NAME_MAX)) {
    return fit::error(errno(ENAMETOOLONG));
  }

  auto child_result = [&]() -> fit::result<Errno, NamespaceNode> {
    if (basename.empty() || basename == ".") {
      return fit::ok(*this);
    } else if (basename == "..") {
      NamespaceNode root;
      switch (context.resolve_base.type) {
        case None:
          root = current_task->fs()->root();
          break;
        case Beneath:
          // Do not allow traversal out of the 'node'.
          if (*this == context.resolve_base.node) {
            return fit::error(errno(EXDEV));
          }
          root = current_task->fs()->root();
          break;
        case InRoot:
          root = context.resolve_base.node;
          break;
      }

      // Make sure this can't escape a chroot.
      if (*this == root) {
        return fit::ok(root);
      }
      return fit::ok(parent().value_or(*this));
    } else {
      auto lookup_result =
          entry_->component_lookup(current_task, this->mount_, basename) _EP(lookup_result);
      auto child = with_new_entry(lookup_result.value());
      while (child.entry_->node_->is_lnk()) {
        bool break_while = false;
        switch (context.symlink_mode) {
          case NoFollow:
            break_while = true;
            break;
          case Follow: {
            if ((context.remaining_follows == 0) ||
                context.resolve_flags.contains(ResolveFlagsEnum::NO_SYMLINKS)) {
              return fit::error(errno(ELOOP));
            }
            context.remaining_follows -= 1;
            auto readlink_result = child.readlink(current_task) _EP(readlink_result);
            auto node = ktl::visit(
                SymlinkTarget::overloaded{
                    [&](const FsString& link_target) -> fit::result<Errno, NamespaceNode> {
                      NamespaceNode link_directory;
                      if (link_target.data()[0] == '/') {
                        switch (context.resolve_base.type) {
                          case None:
                            link_directory = current_task->fs()->root();
                            break;
                          case Beneath:
                            return fit::error(errno(ELOOP));
                          case InRoot:
                            link_directory = context.resolve_base.node;
                            break;
                        }
                        return current_task.lookup_path(context, link_directory,
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
        if (break_while) {
          break;
        }
      }
      return fit::ok(child.enter_mount());
    }
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
ktl::optional<NamespaceNode> NamespaceNode::parent() const { return ktl::nullopt; }

/// Returns the parent, but does not escape mounts i.e. returns None if this node
/// is the root of a mount.
ktl::optional<DirEntryHandle> NamespaceNode::parent_within_mount() const { return ktl::nullopt; }

NamespaceNode NamespaceNode::with_new_entry(DirEntryHandle entry) const {
  return NamespaceNode(this->mount_, ktl::move(entry));
}

NamespaceNode NamespaceNode::enter_mount() const {
  // While the child is a mountpoint, replace child with the mount's root.
  auto enter_one_mount = [](const NamespaceNode& node) -> ktl::optional<NamespaceNode> {
    if (auto mount_opt = node.mount_.handle_; mount_opt.has_value()) {
      // TODO (Herrera) implement
    }
    return ktl::nullopt;
  };

  auto inner = *this;
  while (auto some = enter_one_mount(inner)) {
    inner = some.value();
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
  if (auto _mount = mount_if_root(); _mount.is_ok()) {
    return _mount->mountpoint();
  }
  return ktl::nullopt;
}

FsString NamespaceNode::path(const Task& task) const {
  return path_from_root(task.fs()->root()).into_path();
}

FsString NamespaceNode::path_escaping_chroot() const { return path_from_root({}).into_path(); }

PathWithReachability NamespaceNode::path_from_root(ktl::optional<NamespaceNode> root) const {
  if (auto mount = (*this->mount_); mount.has_value()) {
    return PathWithReachability::Reachable(entry_->local_name());
  }

  auto path = PathBuilder::New();
  auto current = escape_mount();
  if (root.has_value()) {
    // The current node is expected to intersect with the custom root as we travel up the tree.
    auto _root = root->escape_mount();
    while (current != _root) {
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
    auto parent = current.parent();
    while (parent.has_value()) {
      path.prepend_element(current.entry_->local_name());
      parent = parent->escape_mount().parent();
    }
  }

  return PathWithReachability::Reachable(path.build_absolute());
}

fit::result<Errno, SymlinkTarget> NamespaceNode::readlink(const CurrentTask& current_task) const {
  return entry_->node_->readlink(current_task);
}

fit::result<Errno> NamespaceNode::check_access(const CurrentTask& current_task,
                                               Access access) const {
  return fit::ok();
}

fit::result<Errno> NamespaceNode::truncate(const CurrentTask& current_task, uint64_t length) const {
  return fit::error(errno(ENOTSUP));
}

NamespaceNode::~NamespaceNode() { LTRACE_ENTRY_OBJ; }

SymlinkTarget::SymlinkTarget(Variant variant) : variant_(ktl::move(variant)) {}

SymlinkTarget SymlinkTarget::Path(const FsString& path) { return SymlinkTarget(path); }

SymlinkTarget SymlinkTarget::Node(NamespaceNode node) { return SymlinkTarget(node); }

SymlinkTarget::~SymlinkTarget() = default;

}  // namespace starnix

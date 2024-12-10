// Copyright 2024 Mist Tecnologia LTDA
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/mount.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/mount_info.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <trace.h>

#include <utility>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

// A RAII object that unregisters a mount when dropped.
Submount::Submount(DirEntryHandle dir, MountHandle mount)
    : dir_(ktl::move(dir)), mount_(ktl::move(mount)) {
  LTRACE_ENTRY_OBJ;
}

DirEntryHandle Submount::dir() const { return dir_; }

MountHandle Submount::mount() const { return mount_; }

size_t Submount::GetHash(const Submount& key) { return reinterpret_cast<size_t>(key.dir_.get()); }

bool Submount::operator==(const Submount& other) const { return dir_ == other.dir_; }

Submount::~Submount() {
  if (mount_) {
    mount_->fs_->kernel_.Lock()->mounts_.unregister_mount(dir_, mount_);
  }
  LTRACE_EXIT_OBJ;
}

SubmountState::SubmountState(const Submount& submount) : submount_(submount) {}

Submount SubmountState::GetKey() const { return submount_; }

size_t SubmountState::GetHash(const Submount& submount) { return Submount::GetHash(submount); }

MountHandleVector::MountHandleVector(mtl::WeakPtr<DirEntry> key) : key_(ktl::move(key)) {}

mtl::WeakPtr<DirEntry> MountHandleVector::GetKey() const { return key_; }

size_t MountHandleVector::GetHash(const mtl::WeakPtr<DirEntry>& key) {
  return reinterpret_cast<size_t>(key.get());
}

ktl::unique_ptr<SubmountState> Mounts::register_mount(const DirEntryHandle& dir_entry,
                                                      MountHandle mount) const {
  auto mounts = mounts_.Lock();

  auto entry = mounts->find(dir_entry->weak_factory_.GetWeakPtr());
  if (entry == mounts->end()) {
    fbl::AllocChecker ac;
    auto new_vector =
        ktl::make_unique<MountHandleVector>(&ac, dir_entry->weak_factory_.GetWeakPtr());
    ZX_ASSERT(ac.check());
    dir_entry->set_has_mounts(true);
    mounts->insert(ktl::move(new_vector));
  }

  entry = mounts->find(dir_entry->weak_factory_.GetWeakPtr());
  ZX_ASSERT(entry != mounts->end());

  fbl::AllocChecker ac;
  entry->push_back(mount, &ac);
  ZX_ASSERT(ac.check());

  auto sub_mount_state = ktl::make_unique<SubmountState>(&ac, Submount(dir_entry, mount));
  ZX_ASSERT(ac.check());

  return sub_mount_state;
}

void Mounts::unregister_mount(const DirEntryHandle& dir_entry, const MountHandle& mount) const {}

void Mounts::unmount(const DirEntry& dir_entry) const {}

void MountState::add_submount_internal(const DirEntryHandle& dir, MountHandle mount) {
  if (!dir->is_descendant_of(base_->root_)) {
    return;
  }

  auto submount = mount->fs_->kernel_.Lock()->mounts_.register_mount(dir, mount);
  {
    auto mount_state = mount->state_.Write();
    ktl::optional<ktl::pair<mtl::WeakPtr<Mount>, DirEntryHandle>> old_mountpoint =
        ktl::move(mount_state->mountpoint_);
    mount_state->mountpoint_ = ktl::pair(this->base_->weak_factory_.GetWeakPtr(), dir);
    ZX_ASSERT_MSG(!old_mountpoint.has_value(), "add_submount can only take a newly created mount");
  }

  // Mount shadowing is implemented by mounting onto the root of the first mount, not by
  // creating two mounts on the same mountpoint.
  auto old_mount = this->submounts_.insert_or_replace(ktl::move(submount));

  // In rare cases, mount propagation might result in a request to mount on a directory where
  // something is already mounted. MountTest.LotsOfShadowing will trigger this. Linux handles
  // this by inserting the new mount between the old mount and the current mount.
  if (old_mount) {
    // Previous state: self[dir] = old_mount
    // New state: self[dir] = new_mount, new_mount[new_mount.root] = old_mount
    // The new mount has already been inserted into self, now just update the old mount to
    // be a child of the new mount.
    auto old_mount_state = (*old_mount)->mount()->state_.Write();
    old_mount_state->mountpoint_ = ktl::pair(mount->weak_factory_.GetWeakPtr(), dir);
    (*old_mount)->dir_ = mount->root_;
    mount->state_.Write()->submounts_.insert(ktl::move(old_mount));
  }
}

WhatToMount WhatToMount::Fs(FileSystemHandle fs) { return WhatToMount(ktl::move(fs)); }

WhatToMount WhatToMount::Bind(NamespaceNode node) { return WhatToMount(ktl::move(node)); }

WhatToMount::WhatToMount(Variant what) : what_(ktl::move(what)) {}

WhatToMount::~WhatToMount() = default;

MountInfo::~MountInfo() = default;

MountInfo MountInfo::detached() { return {ktl::nullopt}; }

MountFlags MountInfo::flags() const {
  if (handle_.has_value()) {
    return handle_.value()->flags();
  }
  // Consider not mounted node have the NOATIME flags.
  return MountFlags(MountFlagsEnum::NOATIME);
}

fit::result<Errno> MountInfo::check_readonly_filesystem() const {
  if (flags().contains(MountFlagsEnum::RDONLY)) {
    return fit::error(errno(EROFS));
  }
  return fit::ok();
}

fit::result<Errno> MountInfo::check_noexec_filesystem() const {
  if (flags().contains(MountFlagsEnum::NOEXEC)) {
    return fit::error(errno(EACCES));
  }
  return fit::ok();
}

ktl::optional<MountHandle> MountInfo::operator*() const { return handle_; }

NamespaceNode Mount::root() {
  return NamespaceNode{.mount_ = MountInfo{.handle_ = fbl::RefPtr<Mount>(this)}, .entry_ = root_};
}

bool Mount::has_submount(const DirEntryHandle& dir_entry) const {
  auto state = state_.Read();
  Submount key(dir_entry, MountHandle());
  auto entry = state->submounts_.find(key);
  return entry != state->submounts_.end();
}

ktl::optional<NamespaceNode> Mount::mountpoint() const {
  auto state = state_.Read();
  if (!state->mountpoint_.has_value()) {
    return ktl::nullopt;
  }
  auto& [mount, entry] = state->mountpoint_.value();
  auto strong = mount.Lock();
  if (!strong) {
    return ktl::nullopt;
  }
  return NamespaceNode::New(strong, entry);
}

void Mount::create_submount(const DirEntryHandle& dir, WhatToMount what, MountFlags flags) const {
  // TODO(tbodt): Making a copy here is necessary for lock ordering, because the peer group
  // lock nests inside all mount locks (it would be impractical to reverse this because you
  // need to lock a mount to get its peer group.) But it opens the door to race conditions
  // where if a peer are concurrently being added, the mount might not get propagated to the
  // new peer. The only true solution to this is bigger locks, somehow using the same lock
  // for the peer group and all of the mounts in the group. Since peer groups are fluid and
  // can have mounts constantly joining and leaving and then joining other groups, the only
  // sensible locking option is to use a single global lock for all mounts and peer groups.
  // This is almost impossible to express in rust. Help.
  //
  // Update: Also necessary to make a copy to prevent excess replication, see the comment on
  // the following Mount::new call.
  fbl::Vector<MountHandle> peers;
  {
    auto state = this->state_.Read();
    // TODO(Herrera): Implement peer_group() and copy_propagation_targets()
    // peers = state->peer_group().map(|g| g.copy_propagation_targets()).unwrap_or_default();
  }

  // Create the mount after copying the peer groups, because in the case of creating a bind
  // mount inside itself, the new mount would get added to our peer group during the
  // Mount::new call, but we don't want to replicate into it already. For an example see
  // MountTest.QuizBRecursion.
  auto mount = Mount::New(what, flags);

  // TODO(Herrera): Implement is_shared() and make_shared()
  // if (state_.Read().is_shared()) {
  // mount.write().make_shared();
  //}

  for (const auto& peer : peers) {
    if (this == peer.get()) {
      continue;
    }
    auto clone = mount->clone_mount_recursive();
    // TODO(Herrera): Implement add_submount_internal()
    // peer.write().add_submount_internal(dir, clone);
  }

  this->Write()->add_submount_internal(dir, mount);
}

MountHandle Mount::clone_mount(const DirEntryHandle& new_root, MountFlags flags) const {
  ZX_ASSERT(new_root->is_descendant_of(root_));
  //  According to mount(2) on bind mounts, all flags other than MS_REC are ignored when doing
  //  a bind mount.
  auto clone = Mount::new_with_root(new_root, this->flags());

  if (flags.contains(MountFlagsEnum::REC)) {
    // This is two steps because the alternative (locking clone.state while iterating over
    // self.state.submounts) trips tracing_mutex. The lock ordering is parent -> child, and
    // if the clone is eventually made a child of self, this looks like an ordering
    // violation. I'm not convinced it's a real issue, but I can't convince myself it's not
    // either.
    fbl::Vector<ktl::pair<DirEntryHandle, MountHandle>> submounts;
    {
      auto state = state_.Read();
      for (const auto& entry : state->submounts_) {
        fbl::AllocChecker ac;
        submounts.push_back({entry->dir_, entry->mount_->clone_mount_recursive()}, &ac);
        ZX_ASSERT(ac.check());
      }
    }

    auto clone_state = clone->state_.Write();
    for (const auto& [dir, submount] : submounts) {
      clone_state->add_submount_internal(dir, submount);
    }
  }

  // Put the clone in the same peer group
  // TODO(Herrera): Implement peer_group() and set_peer_group()
  // auto peer_group = state_.Read()->peer_group().map(Arc::clone);
  // if (peer_group) {
  //   clone.write().set_peer_group(peer_group);
  // }

  return clone;
}

MountHandle Mount::clone_mount_recursive() const {
  auto self = fbl::RefPtr<const Mount>(this);
  return clone_mount(self->root_, MountFlags(MountFlagsEnum::REC));
}

MountFlags Mount::flags() const { return *flags_.Lock(); }

BString Mount::debug() const {
  auto state = state_.Read();
  auto root_fmt = root_->debug();
  auto mount_fmt = state->mountpoint_.has_value() ? state->mountpoint_->second->debug() : "(none)";

  return mtl::format("Mount {id=%p, root={%.*s}, mountpoint={%.*s}, submounts=%d}", this,
                     static_cast<int>(root_fmt.size()), root_fmt.data(),
                     static_cast<int>(mount_fmt.size()), mount_fmt.data(),
                     state->submounts_.size());
}

fbl::RefPtr<DirEntry> Mount::GetKey() const { return root_; }

MountHandle Mount::New(WhatToMount what, MountFlags flags) {
  return ktl::visit(
      WhatToMount::overloaded{
          [&flags](const FileSystemHandle& fs) { return new_with_root(fs->root(), flags); },
          [&flags](const NamespaceNode& node) {
            ZX_ASSERT_MSG(node.mount_.handle_.has_value(),
                          "can't bind mount from an anonymous node");
            auto mount = node.mount_.handle_.value();
            return mount->clone_mount(node.entry_, flags);
          }},
      what.what_);
}

MountHandle Mount::new_with_root(const DirEntryHandle& root, MountFlags flags) {
  auto known_flags = MountFlags(MountFlagsEnum::STORED_ON_MOUNT);
  ZX_ASSERT_MSG(!flags.intersects(known_flags), "mount created with extra flags %d",
                (flags - known_flags).bits());

  auto fs = root->node_->fs();
  auto kernel = fs->kernel_.Lock();
  ASSERT_MSG(kernel, "can't create mount without a kernel");

  fbl::AllocChecker ac;
  auto handle = fbl::AdoptRef(new (&ac) Mount(kernel->get_next_mount_id(), flags, root, fs));
  ZX_ASSERT(ac.check());
  return handle;
}

Mount::Mount(uint64_t id, MountFlags flags, DirEntryHandle root, FileSystemHandle fs)
    : root_(ktl::move(root)),
      flags_(ktl::move(flags)),
      fs_(ktl::move(fs)),
      id_(id),
      weak_factory_(this) {
  state_.Write()->base_ = this;
}

Mount::~Mount() = default;

}  // namespace starnix

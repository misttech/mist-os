// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/stat.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

zx_status_t Dir::DoCreate(std::string_view name, umode_t mode, fbl::RefPtr<fs::Vnode> *out) {
  auto vnode_or = fs()->CreateNewVnode(safemath::CheckOr<umode_t>(S_IFREG, mode).ValueOrDie());
  if (vnode_or.is_error()) {
    return vnode_or.error_value();
  }

  vnode_or->SetName(name);
  if (!superblock_info_.TestOpt(MountOption::kDisableExtIdentify)) {
    vnode_or->SetColdFile();
  }

  if (zx_status_t err = AddLink(name, (*vnode_or).get()); err != ZX_OK) {
    vnode_or->ClearNlink();
    vnode_or->ClearDirty();
    fs()->GetNodeManager().AddFreeNid(vnode_or->Ino());
    return err;
  }

  vnode_or->ClearFlag(InodeInfoFlag::kNewInode);
  *out = *std::move(vnode_or);
  return ZX_OK;
}

zx::result<> Dir::RecoverLink(VnodeF2fs &vnode) {
  fs::SharedLock lock(f2fs::GetGlobalLock());
  std::lock_guard dir_lock(mutex_);
  fbl::RefPtr<Page> page;
  auto dir_entry = FindEntry(vnode.GetNameView(), &page);
  if (dir_entry.is_error()) {
    AddLink(vnode.GetNameView(), &vnode);
  } else if (vnode.Ino() != dir_entry->ino) {
    // Remove old dentry
    auto old_vnode_or = fs()->GetVnode(dir_entry->ino);
    if (old_vnode_or.is_error()) {
      return old_vnode_or.take_error();
    }
    DeleteEntry(*dir_entry, page, (*old_vnode_or).get());
    ZX_ASSERT(AddLink(vnode.GetNameView(), &vnode) == ZX_OK);
  }
  return zx::ok();
}

zx_status_t Dir::Link(std::string_view name, fbl::RefPtr<fs::Vnode> new_child) {
  if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  if (!fs::IsValidName(name)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fs()->GetSegmentManager().BalanceFs(kMaxNeededBlocksForUpdate);
  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    fbl::RefPtr<VnodeF2fs> target = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(new_child));
    if (target->IsDir()) {
      return ZX_ERR_NOT_FILE;
    }

    std::lock_guard dir_lock(mutex_);
    if (LookUpEntries(name).is_ok()) {
      return ZX_ERR_ALREADY_EXISTS;
    }

    target->SetTime<Timestamps::ChangeTime>();

    target->SetFlag(InodeInfoFlag::kIncLink);
    if (zx_status_t err = AddLink(name, target.get()); err != ZX_OK) {
      target->ClearFlag(InodeInfoFlag::kIncLink);
      return err;
    }
  }

  return ZX_OK;
}

zx_status_t Dir::DoLookup(std::string_view name, fbl::RefPtr<fs::Vnode> *out) {
  if (!fs::IsValidName(name)) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (auto ino_or = LookUpEntries(name); ino_or.is_ok()) {
    auto vnode_or = fs()->GetVnode(*ino_or);
    if (vnode_or.is_error()) {
      return vnode_or.error_value();
    }
    *out = *std::move(vnode_or);
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t Dir::Lookup(std::string_view name, fbl::RefPtr<fs::Vnode> *out) {
  fs::SharedLock dir_read_lock(mutex_);
  return DoLookup(name, out);
}

zx_status_t Dir::DoUnlink(VnodeF2fs *vnode, std::string_view name) {
  fbl::RefPtr<Page> page;
  auto entry_info = FindEntry(name, &page);
  if (entry_info.is_error()) {
    return ZX_ERR_NOT_FOUND;
  }
  if (zx_status_t err = fs()->CheckOrphanSpace(); err != ZX_OK) {
    return err;
  }

  DeleteEntry(*entry_info, page, vnode);
  return ZX_OK;
}

#if 0  // porting needed
// int Dir::F2fsSymlink(dentry *dentry, const char *symname) {
//   return 0;
//   //   fbl::RefPtr<VnodeF2fs> vnode_refptr;
//   //   VnodeF2fs *vnode = nullptr;
//   //   unsigned symlen = strlen(symname) + 1;
//   //   int err;

//   //   err = NewInode(S_IFLNK | S_IRWXUGO, &vnode_refptr);
//   //   if (err)
//   //     return err;
//   //   vnode = vnode_refptr.get();

//   //   // inode->i_mapping->a_ops = &f2fs_dblock_aops;

//   //   // err = AddLink(dentry, vnode);
//   //   if (err)
//   //     goto out;

//   //   err = page_symlink(vnode, symname, symlen);
//   //   fs()->GetNodeManager().AllocNidDone(vnode->Ino());

//   //   // d_instantiate(dentry, vnode);
//   //   UnlockNewInode(vnode);

//   //   fs()->GetSegmentManager().BalanceFs();

//   //   return err;
//   // out:
//   //   vnode->ClearNlink();
//   //   UnlockNewInode(vnode);
//   //   fs()->GetNodeManager().AllocNidFailed(vnode->Ino());
//   //   return err;
// }
#endif

zx_status_t Dir::Mkdir(std::string_view name, umode_t mode, fbl::RefPtr<fs::Vnode> *out) {
  auto vnode_or = fs()->CreateNewVnode(safemath::CheckOr<umode_t>(S_IFDIR, mode).ValueOrDie());
  if (vnode_or.is_error()) {
    return vnode_or.error_value();
  }
  vnode_or->SetName(name);
  vnode_or->SetFlag(InodeInfoFlag::kIncLink);
  if (zx_status_t err = AddLink(name, (*vnode_or).get()); err != ZX_OK) {
    vnode_or->ClearFlag(InodeInfoFlag::kIncLink);
    vnode_or->ClearNlink();
    vnode_or->ClearDirty();
    fs()->GetNodeManager().AddFreeNid(vnode_or->Ino());
    return err;
  }
  vnode_or->ClearFlag(InodeInfoFlag::kNewInode);
  *out = *std::move(vnode_or);
  return ZX_OK;
}

zx_status_t Dir::Rmdir(Dir *vnode, std::string_view name) {
  if (vnode->IsEmptyDir()) {
    return DoUnlink(vnode, name);
  }
  return ZX_ERR_NOT_EMPTY;
}

#if 0  // porting needed
// int Dir::F2fsMknod(dentry *dentry, umode_t mode, dev_t rdev) {
//   fbl::RefPtr<VnodeF2fs> vnode_refptr;
//   VnodeF2fs *vnode = nullptr;
//   int err = 0;

//   // if (!new_valid_dev(rdev))
//   //   return -EINVAL;

//   err = NewInode(mode, &vnode_refptr);
//   if (err)
//     return err;
//   vnode = vnode_refptr.get();

//   // init_special_inode(inode, inode->i_mode, rdev);
//   // inode->i_op = &f2fs_special_inode_operations;

//   // err = AddLink(dentry, vnode);
//   if (err)
//     goto out;

//   fs()->GetNodeManager().AllocNidDone(vnode->Ino());
//   // d_instantiate(dentry, inode);
//   UnlockNewInode(vnode);

//   fs()->GetSegmentManager().BalanceFs();

//   return 0;
// out:
//   vnode->ClearNlink();
//   UnlockNewInode(vnode);
//   fs()->GetNodeManager().AllocNidFailed(vnode->Ino());
//   return err;
// }
#endif

zx::result<bool> Dir::IsSubdir(Dir *possible_dir) {
  VnodeF2fs *vnode = possible_dir;
  while (vnode->Ino() != superblock_info_.GetRootIno()) {
    if (vnode->Ino() == Ino()) {
      return zx::ok(true);
    }

    auto parent_or = fs()->GetVnode(vnode->GetParentNid());
    if (parent_or.is_error()) {
      return parent_or.take_error();
    }
    vnode = (*parent_or).get();
  }
  return zx::ok(false);
}

zx_status_t Dir::Rename(fbl::RefPtr<fs::Vnode> _newdir, std::string_view oldname,
                        std::string_view newname, bool src_must_be_dir, bool dst_must_be_dir) {
  if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  fs()->GetSegmentManager().BalanceFs(2 * kMaxNeededBlocksForUpdate + 1);
  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    fbl::RefPtr<Dir> new_dir = fbl::RefPtr<Dir>::Downcast(std::move(_newdir));
    bool is_same_dir = (new_dir.get() == this);
    if (!fs::IsValidName(oldname) || !fs::IsValidName(newname)) {
      return ZX_ERR_INVALID_ARGS;
    }

    std::lock_guard dir_lock(mutex_);
    if (new_dir->GetNlink() == 0) {
      return ZX_ERR_NOT_FOUND;
    }

    fbl::RefPtr<Page> old_page;
    auto old_entry = FindEntry(oldname, &old_page);
    if (old_entry.is_error()) {
      return ZX_ERR_NOT_FOUND;
    }

    nid_t old_ino = old_entry->ino;
    auto old_vnode_or = fs()->GetVnode(old_ino);
    if (old_vnode_or.is_error()) {
      return old_vnode_or.error_value();
    }

    fbl::RefPtr<VnodeF2fs> old_vnode = *std::move(old_vnode_or);
    ZX_DEBUG_ASSERT(old_vnode->IsSameName(oldname));

    if (!old_vnode->IsDir() && (src_must_be_dir || dst_must_be_dir)) {
      return ZX_ERR_NOT_DIR;
    }

    ZX_DEBUG_ASSERT(!src_must_be_dir || old_vnode->IsDir());

    fbl::RefPtr<Page> old_dir_page;
    zx::result<DentryInfo> old_dir_entry = zx::error(ZX_ERR_NOT_FOUND);
    if (old_vnode->IsDir()) {
      old_dir_entry = fbl::RefPtr<Dir>::Downcast(old_vnode)->GetParentDentryInfo(&old_dir_page);
      if (old_dir_entry.is_error()) {
        return ZX_ERR_IO;
      }

      auto is_subdir = fbl::RefPtr<Dir>::Downcast(old_vnode)->IsSubdir(new_dir.get());
      if (is_subdir.is_error()) {
        return is_subdir.error_value();
      }
      if (*is_subdir) {
        return ZX_ERR_INVALID_ARGS;
      }
    }

    fbl::RefPtr<Page> new_page;
    zx::result<DentryInfo> new_entry = zx::error(ZX_ERR_NOT_FOUND);
    if (is_same_dir) {
      new_entry = FindEntry(newname, &new_page);
    } else {
      new_entry = new_dir->FindEntrySafe(newname, &new_page);
    }

    if (new_entry.is_ok()) {
      ino_t new_ino = new_entry->ino;
      auto new_vnode_or = fs()->GetVnode(new_ino);
      if (new_vnode_or.is_error()) {
        return new_vnode_or.error_value();
      }

      fbl::RefPtr<VnodeF2fs> new_vnode = *std::move(new_vnode_or);
      if (!new_vnode->IsDir() && (src_must_be_dir || dst_must_be_dir)) {
        return ZX_ERR_NOT_DIR;
      }

      if (old_vnode->IsDir() && !new_vnode->IsDir()) {
        return ZX_ERR_NOT_DIR;
      }

      if (!old_vnode->IsDir() && new_vnode->IsDir()) {
        return ZX_ERR_NOT_FILE;
      }

      if (is_same_dir && oldname == newname) {
        return ZX_OK;
      }

      if (old_dir_entry.is_ok() &&
          (!new_vnode->IsDir() || !fbl::RefPtr<Dir>::Downcast(new_vnode)->IsEmptyDir())) {
        return ZX_ERR_NOT_EMPTY;
      }

      ZX_DEBUG_ASSERT(this != new_vnode.get());
      ZX_DEBUG_ASSERT(new_vnode->IsSameName(newname));
      old_vnode->SetName(newname);
      if (is_same_dir) {
        SetLink(*new_entry, new_page, old_vnode.get());
      } else {
        new_dir->SetLinkSafe(*new_entry, new_page, old_vnode.get());
      }

      new_vnode->SetTime<Timestamps::ChangeTime>();
      if (old_dir_entry.is_ok()) {
        new_vnode->DropNlink();
      }
      new_vnode->DropNlink();
      if (!new_vnode->GetNlink()) {
        new_vnode->SetOrphan();
      }
      new_vnode->SetDirty();
    } else {
      if (is_same_dir && oldname == newname) {
        return ZX_OK;
      }

      old_vnode->SetName(newname);

      if (is_same_dir) {
        if (zx_status_t err = AddLink(newname, old_vnode.get()); err != ZX_OK) {
          return err;
        }
        if (old_dir_entry.is_ok()) {
          IncNlink();
          SetDirty();
        }
      } else {
        if (zx_status_t err = new_dir->AddLinkSafe(newname, old_vnode.get()); err != ZX_OK) {
          return err;
        }
        if (old_dir_entry.is_ok()) {
          new_dir->IncNlink();
          new_dir->SetDirty();
        }
      }
    }

    old_vnode->SetParentNid(new_dir->Ino());
    old_vnode->SetTime<Timestamps::ChangeTime>();
    old_vnode->SetFlag(InodeInfoFlag::kNeedCp);
    old_vnode->SetDirty();

    DeleteEntry(*old_entry, old_page, nullptr);

    if (old_dir_entry.is_ok()) {
      if (!is_same_dir) {
        fbl::RefPtr<Dir>::Downcast(old_vnode)->SetLinkSafe(*old_dir_entry, old_dir_page,
                                                           new_dir.get());
      }
      DropNlink();
      SetDirty();
    }

    // Add new parent directory to VnodeSet to ensure consistency of renamed vnode.
    fs()->AddToVnodeSet(VnodeSet::kModifiedDir, new_dir->Ino());
    if (old_vnode->IsDir()) {
      fs()->AddToVnodeSet(VnodeSet::kModifiedDir, old_vnode->Ino());
    }
  }

  return ZX_OK;
}

zx::result<fbl::RefPtr<fs::Vnode>> Dir::Create(std::string_view name, fs::CreationType type) {
  switch (type) {
    case fs::CreationType::kDirectory:
      return CreateWithMode(name, S_IFDIR);
    case fs::CreationType::kFile:
      return CreateWithMode(name, S_IFREG);
  }
}

zx::result<fbl::RefPtr<fs::Vnode>> Dir::CreateWithMode(std::string_view name, umode_t mode) {
  if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  if (!fs::IsValidName(name)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fs()->GetSegmentManager().BalanceFs(kMaxNeededBlocksForUpdate + 1);
  fbl::RefPtr<fs::Vnode> new_vnode;
  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    std::lock_guard dir_lock(mutex_);
    if (GetNlink() == 0)
      return zx::error(ZX_ERR_NOT_FOUND);

    if (LookUpEntries(name).is_ok()) {
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }

    if (S_ISDIR(mode)) {
      if (zx_status_t status = Mkdir(name, mode, &new_vnode); status != ZX_OK) {
        return zx::error(status);
      }
    } else {
      if (zx_status_t status = DoCreate(name, mode, &new_vnode); status != ZX_OK) {
        return zx::error(status);
      }
    }
  }
  return zx::make_result(new_vnode->Open(nullptr), new_vnode);
}

zx_status_t Dir::Unlink(std::string_view name, bool must_be_dir) {
  if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }
  fbl::RefPtr<fs::Vnode> vnode;
  if (zx_status_t status = Lookup(name, &vnode); status != ZX_OK) {
    return status;
  }
  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    std::lock_guard dir_lock(mutex_);
    fbl::RefPtr<VnodeF2fs> removed = fbl::RefPtr<VnodeF2fs>::Downcast(std::move(vnode));
    zx_status_t ret;
    if (removed->IsDir()) {
      fbl::RefPtr<Dir> dir = fbl::RefPtr<Dir>::Downcast(std::move(removed));
      ret = Rmdir(dir.get(), name);
    } else if (must_be_dir) {
      return ZX_ERR_NOT_DIR;
    } else {
      ret = DoUnlink(removed.get(), name);
    }
    if (ret != ZX_OK) {
      return ret;
    }
  }
  return ZX_OK;
}

}  // namespace f2fs

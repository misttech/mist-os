// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/stat.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

zx_status_t Dir::NewInode(umode_t mode, fbl::RefPtr<VnodeF2fs> *out) {
  fbl::RefPtr<VnodeF2fs> vnode;

  auto ino_or = fs()->GetNodeManager().AllocNid();
  if (ino_or.is_error()) {
    return ZX_ERR_NO_SPACE;
  }

  if (auto ret = VnodeF2fs::Allocate(fs(), *ino_or, mode, &vnode); ret != ZX_OK) {
    return ret;
  }
  vnode->SetUid(getuid());
  if (HasGid()) {
    vnode->SetGid(gid_);
    if (S_ISDIR(mode))
      mode |= S_ISGID;
  } else {
    vnode->SetGid(getgid());
  }

  vnode->InitNlink();
  vnode->InitBlocks();
  vnode->InitTime();
  vnode->SetGeneration(superblock_info_.GetNextGeneration());
  superblock_info_.IncNextGeneration();

  if (superblock_info_.TestOpt(MountOption::kInlineDentry) && vnode->IsDir()) {
    vnode->SetFlag(InodeInfoFlag::kInlineDentry);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }

  if (vnode->IsReg()) {
    vnode->InitExtentTree();
  }
  vnode->InitFileCache();
  vnode->SetFlag(InodeInfoFlag::kNewInode);

  fs()->InsertVnode(vnode.get());
  vnode->SetDirty();

  *out = std::move(vnode);
  return ZX_OK;
}

zx_status_t Dir::DoCreate(std::string_view name, umode_t mode, fbl::RefPtr<fs::Vnode> *out) {
  fbl::RefPtr<VnodeF2fs> vnode_refptr;
  VnodeF2fs *vnode = nullptr;

  if (zx_status_t err =
          NewInode(safemath::CheckOr<umode_t>(S_IFREG, mode).ValueOrDie(), &vnode_refptr);
      err != ZX_OK) {
    return err;
  }

  vnode = vnode_refptr.get();
  vnode->SetName(name);

  if (!superblock_info_.TestOpt(MountOption::kDisableExtIdentify))
    vnode->SetColdFile();

  if (zx_status_t err = AddLink(name, vnode); err != ZX_OK) {
    vnode->ClearNlink();
    vnode->UnlockNewInode();
    vnode->ClearDirty();
    fs()->GetNodeManager().AddFreeNid(vnode->Ino());
    return err;
  }

  vnode->UnlockNewInode();
  *out = std::move(vnode_refptr);
  return ZX_OK;
}

zx::result<> Dir::RecoverLink(VnodeF2fs &vnode) {
  fs::SharedLock lock(f2fs::GetGlobalLock());
  std::lock_guard dir_lock(mutex_);
  fbl::RefPtr<Page> page;
  auto dir_entry = FindEntry(vnode.GetNameView(), &page);
  if (dir_entry == nullptr) {
    AddLink(vnode.GetNameView(), &vnode);
  } else if (dir_entry && vnode.Ino() != LeToCpu(dir_entry->ino)) {
    // Remove old dentry
    fbl::RefPtr<VnodeF2fs> old_vnode_refptr;
    if (zx_status_t err = VnodeF2fs::Vget(fs(), dir_entry->ino, &old_vnode_refptr); err != ZX_OK) {
      return zx::error(err);
    }
    DeleteEntry(dir_entry, page, old_vnode_refptr.get());
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
    if (auto old_entry = FindEntry(name); old_entry.is_ok()) {
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
  fbl::RefPtr<VnodeF2fs> vn;

  if (!fs::IsValidName(name)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (auto dir_entry = LookUpEntries(name); !dir_entry.is_error()) {
    nid_t ino = LeToCpu((*dir_entry).ino);
    if (zx_status_t ret = VnodeF2fs::Vget(fs(), ino, &vn); ret != ZX_OK)
      return ret;

    *out = std::move(vn);

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
  DirEntry *de = FindEntry(name, &page);
  if (de == nullptr) {
    return ZX_ERR_NOT_FOUND;
  }
  if (zx_status_t err = fs()->CheckOrphanSpace(); err != ZX_OK) {
    return err;
  }

  DeleteEntry(de, page, vnode);
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
  fbl::RefPtr<VnodeF2fs> vnode_refptr;
  VnodeF2fs *vnode = nullptr;

  if (zx_status_t err =
          NewInode(safemath::CheckOr<umode_t>(S_IFDIR, mode).ValueOrDie(), &vnode_refptr);
      err != ZX_OK)
    return err;
  vnode = vnode_refptr.get();
  vnode->SetName(name);
  vnode->SetFlag(InodeInfoFlag::kIncLink);
  if (zx_status_t err = AddLink(name, vnode); err != ZX_OK) {
    vnode->ClearFlag(InodeInfoFlag::kIncLink);
    vnode->ClearNlink();
    vnode->UnlockNewInode();
    vnode->ClearDirty();
    fs()->GetNodeManager().AddFreeNid(vnode->Ino());
    return err;
  }
  vnode->UnlockNewInode();

  *out = std::move(vnode_refptr);
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
  fbl::RefPtr<VnodeF2fs> parent;
  while (vnode->Ino() != superblock_info_.GetRootIno()) {
    if (vnode->Ino() == Ino()) {
      return zx::ok(true);
    }

    if (zx_status_t status = VnodeF2fs::Vget(fs(), vnode->GetParentNid(), &parent);
        status != ZX_OK) {
      return zx::error(status);
    }

    vnode = parent.get();
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
    DirEntry *old_entry = FindEntry(oldname, &old_page);
    if (!old_entry) {
      return ZX_ERR_NOT_FOUND;
    }

    fbl::RefPtr<VnodeF2fs> old_vnode;
    nid_t old_ino = LeToCpu(old_entry->ino);
    if (zx_status_t err = VnodeF2fs::Vget(fs(), old_ino, &old_vnode); err != ZX_OK) {
      return err;
    }

    ZX_DEBUG_ASSERT(old_vnode->IsSameName(oldname));

    if (!old_vnode->IsDir() && (src_must_be_dir || dst_must_be_dir)) {
      return ZX_ERR_NOT_DIR;
    }

    ZX_DEBUG_ASSERT(!src_must_be_dir || old_vnode->IsDir());

    fbl::RefPtr<Page> old_dir_page;
    DirEntry *old_dir_entry = nullptr;
    if (old_vnode->IsDir()) {
      old_dir_entry = fbl::RefPtr<Dir>::Downcast(old_vnode)->ParentDir(&old_dir_page);
      if (!old_dir_entry) {
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
    DirEntry *new_entry = nullptr;
    if (is_same_dir) {
      new_entry = FindEntry(newname, &new_page);
    } else {
      new_entry = new_dir->FindEntrySafe(newname, &new_page);
    }

    if (new_entry) {
      ino_t new_ino = LeToCpu(new_entry->ino);
      fbl::RefPtr<VnodeF2fs> new_vnode;
      if (zx_status_t err = VnodeF2fs::Vget(fs(), new_ino, &new_vnode); err != ZX_OK) {
        return err;
      }

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

      if (old_dir_entry &&
          (!new_vnode->IsDir() || !fbl::RefPtr<Dir>::Downcast(new_vnode)->IsEmptyDir())) {
        return ZX_ERR_NOT_EMPTY;
      }

      ZX_DEBUG_ASSERT(this != new_vnode.get());
      ZX_DEBUG_ASSERT(new_vnode->IsSameName(newname));
      old_vnode->SetName(newname);
      if (is_same_dir) {
        SetLink(new_entry, new_page, old_vnode.get());
      } else {
        new_dir->SetLinkSafe(new_entry, new_page, old_vnode.get());
      }

      new_vnode->SetTime<Timestamps::ChangeTime>();
      if (old_dir_entry) {
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
        if (old_dir_entry) {
          IncNlink();
          SetDirty();
        }
      } else {
        if (zx_status_t err = new_dir->AddLinkSafe(newname, old_vnode.get()); err != ZX_OK) {
          return err;
        }
        if (old_dir_entry) {
          new_dir->IncNlink();
          new_dir->SetDirty();
        }
      }
    }

    old_vnode->SetParentNid(new_dir->Ino());
    old_vnode->SetTime<Timestamps::ChangeTime>();
    old_vnode->SetFlag(InodeInfoFlag::kNeedCp);
    old_vnode->SetDirty();

    DeleteEntry(old_entry, old_page, nullptr);

    if (old_dir_entry) {
      if (!is_same_dir) {
        fbl::RefPtr<Dir>::Downcast(old_vnode)->SetLinkSafe(old_dir_entry, old_dir_page,
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

zx_status_t Dir::Create(std::string_view name, uint32_t mode, fbl::RefPtr<fs::Vnode> *out) {
  if (superblock_info_.TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  if (!fs::IsValidName(name)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fs()->GetSegmentManager().BalanceFs(kMaxNeededBlocksForUpdate + 1);
  {
    fs::SharedLock lock(f2fs::GetGlobalLock());
    std::lock_guard dir_lock(mutex_);
    if (GetNlink() == 0)
      return ZX_ERR_NOT_FOUND;

    if (auto ret = FindEntry(name); !ret.is_error()) {
      return ZX_ERR_ALREADY_EXISTS;
    }

    zx_status_t ret = ZX_OK;
    if (S_ISDIR(mode)) {
      ret = Mkdir(name, safemath::checked_cast<umode_t>(mode), out);
    } else {
      ret = DoCreate(name, safemath::checked_cast<umode_t>(mode), out);
    }
    if (ret != ZX_OK) {
      return ret;
    }
  }

  return (*out)->Open(nullptr);
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

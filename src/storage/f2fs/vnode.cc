// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/stat.h>
#include <zircon/syscalls-next.h>

#include "src/storage/f2fs/f2fs.h"

namespace f2fs {

VnodeF2fs::VnodeF2fs(F2fs *fs, ino_t ino, umode_t mode)
    : PagedVnode(*fs->vfs()), ino_(ino), fs_(fs), mode_(mode) {
  if (IsMeta() || IsNode()) {
    InitFileCache();
  }
}

fs::VnodeProtocolSet VnodeF2fs::GetProtocols() const { return fs::VnodeProtocol::kFile; }

void VnodeF2fs::SetMode(const umode_t &mode) { mode_ = mode; }

umode_t VnodeF2fs::GetMode() const { return mode_; }

bool VnodeF2fs::IsDir() const { return S_ISDIR(mode_); }

bool VnodeF2fs::IsReg() const { return S_ISREG(mode_); }

bool VnodeF2fs::IsLink() const { return S_ISLNK(mode_); }

bool VnodeF2fs::IsChr() const { return S_ISCHR(mode_); }

bool VnodeF2fs::IsBlk() const { return S_ISBLK(mode_); }

bool VnodeF2fs::IsSock() const { return S_ISSOCK(mode_); }

bool VnodeF2fs::IsFifo() const { return S_ISFIFO(mode_); }

bool VnodeF2fs::HasGid() const { return mode_ & S_ISGID; }

bool VnodeF2fs::IsNode() const {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();
  return ino_ == superblock_info.GetNodeIno();
}

bool VnodeF2fs::IsMeta() const {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();
  return ino_ == superblock_info.GetMetaIno();
}

zx_status_t VnodeF2fs::GetNodeInfoForProtocol([[maybe_unused]] fs::VnodeProtocol protocol,
                                              [[maybe_unused]] fs::Rights rights,
                                              fs::VnodeRepresentation *info) {
  *info = fs::VnodeRepresentation::File();
  return ZX_OK;
}

zx_status_t VnodeF2fs::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo *out_vmo) {
  if (!IsReg()) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (flags & fuchsia_io::wire::VmoFlags::kExecute) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  if ((flags & fuchsia_io::wire::VmoFlags::kSharedBuffer) &&
      (flags & fuchsia_io::wire::VmoFlags::kWrite)) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  std::lock_guard lock(mutex_);
  auto size_or = CreatePagedVmo();
  if (size_or.is_error()) {
    return size_or.error_value();
  }

  return ClonePagedVmo(flags, *size_or, out_vmo);
}

zx::result<size_t> VnodeF2fs::CreatePagedVmo() {
  ZX_ASSERT(PAGE_SIZE == kBlockSize);
  constexpr size_t granularity = PAGE_SIZE;
  size_t rounded_size = fbl::round_up(GetSize(), granularity);
  if (rounded_size == 0) {
    rounded_size = granularity;
  }
  if (!paged_vmo().is_valid()) {
    // TODO: consider updating the content size of vmo.
    if (auto status = EnsureCreatePagedVmo(rounded_size, ZX_VMO_RESIZABLE | ZX_VMO_TRAP_DIRTY);
        status.is_error()) {
      return status.take_error();
    }
    SetPagedVmoName();
  }
  return zx::ok(rounded_size);
}

void VnodeF2fs::SetPagedVmoName() {
  fbl::StringBuffer<ZX_MAX_NAME_LEN> name;
  name.Clear();
  name.AppendPrintf("%s-%.8s-%u", "f2fs", GetNameView().data(), GetKey());
  paged_vmo().set_property(ZX_PROP_NAME, name.data(), name.size());
}

zx_status_t VnodeF2fs::ClonePagedVmo(fuchsia_io::wire::VmoFlags flags, size_t size,
                                     zx::vmo *out_vmo) {
  if (!paged_vmo()) {
    return ZX_ERR_NOT_FOUND;
  }

  zx_rights_t rights = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHT_GET_PROPERTY;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kRead) ? ZX_RIGHT_READ : 0;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kWrite) ? ZX_RIGHT_WRITE : 0;

  zx::vmo vmo;
  if (flags & fuchsia_io::wire::VmoFlags::kSharedBuffer) {
    if (auto status = paged_vmo().duplicate(rights, &vmo); status != ZX_OK) {
      FX_LOGS(ERROR) << "Faild to duplicate VMO: " << zx_status_get_string(status);
      return status;
    }
  } else {
    uint32_t options = ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE;
    if (!(flags & fuchsia_io::wire::VmoFlags::kWrite)) {
      options |= ZX_VMO_CHILD_NO_WRITE;
    }
    if (auto status = paged_vmo().create_child(options, 0, size, &vmo); status != ZX_OK) {
      FX_LOGS(ERROR) << "Faild to create child VMO: " << zx_status_get_string(status);
      return status;
    }
    DidClonePagedVmo();
    rights |= ZX_RIGHT_SET_PROPERTY;
    if (auto status = vmo.replace(rights, &vmo); status != ZX_OK) {
      return status;
    }
  }
  *out_vmo = std::move(vmo);
  return ZX_OK;
}

void VnodeF2fs::VmoRead(uint64_t offset, uint64_t length) {
  ZX_DEBUG_ASSERT(kBlockSize == PAGE_SIZE);
  ZX_DEBUG_ASSERT(offset % kBlockSize == 0);
  ZX_DEBUG_ASSERT(length);
  ZX_DEBUG_ASSERT(length % kBlockSize == 0);

  zx::vmo vmo;
  auto size_or = CreateAndPopulateVmo(vmo, offset, length);
  fs::SharedLock rlock(mutex_);
  if (unlikely(!paged_vmo())) {
    // Races with calling FreePagedVmo() on another thread can result in stale read requests. Ignore
    // them if the VMO is gone.
    FX_LOGS(WARNING) << "A pager-backed VMO is already freed: " << ZX_ERR_NOT_FOUND;
    return;
  }

  if (unlikely(size_or.is_error())) {
    FX_LOGS(ERROR) << "failed to popluate vmo at " << offset << " + " << length << ", "
                   << size_or.status_string();
    ReportPagerError(offset, length, ZX_ERR_BAD_STATE);
    return;
  }
  if (auto ret = vmo_manager_->SupplyPages(*fs()->vfs(), offset, *size_or, std::move(vmo), 0);
      ret.is_error()) {
    ReportPagerError(offset, length, ZX_ERR_BAD_STATE);
  }
}

zx::result<size_t> VnodeF2fs::CreateAndPopulateVmo(zx::vmo &vmo, const size_t offset,
                                                   const size_t length) {
  size_t vmo_size = 0;
  // do readahead if |scan_blocks| of active pages are alive before |index| in order.
  size_t scan_blocks = 4UL;
  std::vector<bool> block_bitmap;
  size_t block_offset = offset / kBlockSize;

  if (TestFlag(InodeInfoFlag::kInlineData) || offset >= GetSize() ||
      TestFlag(InodeInfoFlag::kNoAlloc)) {
    // Just supply pages without read I/Os for appended or inline area.
    // |vmo_size| is set to kDefaultReadaheadSize by default
    // unless supplying |*vmo_ro| exceeds the boundary of paged vmo node.
    vmo_size =
        std::min(fbl::round_up(offset + length, kBlockSize * kDefaultBlocksPerSegment) - offset,
                 kBlockSize * kDefaultReadaheadSize);
  } else {
    block_bitmap = GetReadaheadPagesInfo(block_offset, scan_blocks);
    ZX_DEBUG_ASSERT(block_bitmap.size() >= length / kBlockSize);
    vmo_size =
        std::min(fbl::round_up(offset + length, kBlockSize * kDefaultBlocksPerSegment) - offset,
                 block_bitmap.size() * kBlockSize);
  }

  // Create vmo to feed paged vmo.
  if (auto status = zx::vmo::create(vmo_size, 0, &vmo); status != ZX_OK) {
    return zx::error(status);
  }

  // Commit pages to |vmo|. Ensure that newly commited pages are set to zero.
  if (auto ret = vmo.op_range(ZX_VMO_OP_COMMIT, 0, vmo_size, nullptr, 0); ret != ZX_OK) {
    return zx::error(ret);
  }

  // It tries read IOs only for valid block addrs unless regarding pages are subject to writeback.
  if (block_bitmap.size()) {
    size_t block_length = vmo_size / kBlockSize;
    auto addrs =
        fs()->GetNodeManager().GetDataBlockAddresses(*this, block_offset, block_length, true);
    if (addrs.is_error()) {
      return addrs.take_error();
    }
    auto i = 0;
    auto read_blocks = 0;
    for (auto &addr : *addrs) {
      if (!block_bitmap[i++]) {
        // no read IOs for dirty and writeback pages.
        addr = kNullAddr;
      } else if (addr != kNewAddr && addr != kNullAddr) {
        // the number of blocks to be read.
        ++read_blocks;
      }
    }
    if (read_blocks) {
      if (auto status = fs()->MakeReadOperations(vmo, *addrs, PageType::kData); status.is_error()) {
        return status.take_error();
      }
    }
  }
  return zx::ok(vmo_size);
}

void VnodeF2fs::VmoDirty(uint64_t offset, uint64_t length) {
  auto ret = vmo_manager().DirtyPages(*fs()->vfs(), offset, offset + length);
  if (unlikely(ret.is_error())) {
    FX_LOGS(WARNING) << "failed to do ZX_PAGER_OP_DIRTY from " << offset << " to "
                     << offset + length << ". " << ret.status_string();
    fs::SharedLock lock(mutex_);
    ReportPagerError(offset, length, ret.status_value());
  }
}

void VnodeF2fs::OnNoPagedVmoClones() {
  // Override PagedVnode::OnNoPagedVmoClones().
  // We intend to keep PagedVnode::paged_vmo alive while this vnode has any reference.
  ZX_DEBUG_ASSERT(!has_clones());
}

void VnodeF2fs::ReportPagerError(const uint64_t offset, const uint64_t length,
                                 const zx_status_t err) {
  std::optional vfs = this->vfs();
  ZX_ASSERT(vfs.has_value());
  if (auto result = vfs.value().get().ReportPagerError(paged_vmo(), offset, length, err);
      result.is_error()) {
    FX_LOGS(ERROR) << "Failed to report pager error to kernel: " << result.status_string();
  }
}

zx_status_t VnodeF2fs::Allocate(F2fs *fs, ino_t ino, umode_t mode, fbl::RefPtr<VnodeF2fs> *out) {
  if (fs->GetNodeManager().CheckNidRange(ino)) {
    if (S_ISDIR(mode)) {
      *out = fbl::MakeRefCounted<Dir>(fs, ino, mode);
    } else {
      *out = fbl::MakeRefCounted<File>(fs, ino, mode);
    }
    (*out)->Init();
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t VnodeF2fs::Create(F2fs *fs, ino_t ino, fbl::RefPtr<VnodeF2fs> *out) {
  LockedPage node_page;
  if (ino < fs->GetSuperblockInfo().GetRootIno() || !fs->GetNodeManager().CheckNidRange(ino) ||
      fs->GetNodeManager().GetNodePage(ino, &node_page) != ZX_OK) {
    return ZX_ERR_NOT_FOUND;
  }

  Inode &ri = node_page->GetAddress<Node>()->i;
  fbl::RefPtr<VnodeF2fs> vnode;
  if (auto status = Allocate(fs, ino, LeToCpu(ri.i_mode), &vnode); status != ZX_OK) {
    return status;
  }

  vnode->SetUid(LeToCpu(ri.i_uid));
  vnode->SetGid(LeToCpu(ri.i_gid));
  vnode->SetNlink(LeToCpu(ri.i_links));
  vnode->SetSize(LeToCpu(ri.i_size));
  // Don't count the in-memory inode.i_blocks for compatibility with the generic filesystem
  // including linux f2fs.
  vnode->SetBlocks(safemath::CheckSub<uint64_t>(LeToCpu(ri.i_blocks), 1).ValueOrDie());
  vnode->SetATime(LeToCpu(ri.i_atime), LeToCpu(ri.i_atime_nsec));
  vnode->SetCTime(LeToCpu(ri.i_ctime), LeToCpu(ri.i_ctime_nsec));
  vnode->SetMTime(LeToCpu(ri.i_mtime), LeToCpu(ri.i_mtime_nsec));
  vnode->SetGeneration(LeToCpu(ri.i_generation));
  vnode->SetParentNid(LeToCpu(ri.i_pino));
  vnode->SetCurDirDepth(LeToCpu(ri.i_current_depth));
  vnode->SetXattrNid(LeToCpu(ri.i_xattr_nid));
  vnode->SetInodeFlags(LeToCpu(ri.i_flags));
  vnode->SetDirLevel(ri.i_dir_level);
  vnode->UpdateVersion(LeToCpu(fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver) - 1);
  vnode->SetAdvise(ri.i_advise);
  vnode->GetExtentInfo(ri.i_ext);
  std::string_view name(reinterpret_cast<char *>(ri.i_name), std::min(kMaxNameLen, ri.i_namelen));
  if (ri.i_namelen != name.length() ||
      (ino != fs->GetSuperblockInfo().GetRootIno() && !fs::IsValidName(name))) {
    // TODO: Need to repair the file or set NeedFsck flag when fsck supports repair feature.
    // For now, we set kBad and clear link, so that it can be deleted without purging.
    vnode->ClearNlink();
    vnode->SetFlag(InodeInfoFlag::kBad);
    return ZX_ERR_NOT_FOUND;
  }

  vnode->SetName(name);
  vnode->InitFileCache();

  if (ri.i_inline & kInlineDentry) {
    vnode->SetFlag(InodeInfoFlag::kInlineDentry);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }
  if (ri.i_inline & kInlineData) {
    vnode->SetFlag(InodeInfoFlag::kInlineData);
  }
  if (ri.i_inline & kInlineXattr) {
    vnode->SetFlag(InodeInfoFlag::kInlineXattr);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }
  if (ri.i_inline & kExtraAttr) {
    vnode->SetExtraISize(ri.i_extra_isize);
    if (ri.i_inline & kInlineXattr) {
      vnode->SetInlineXattrAddrs(ri.i_inline_xattr_size);
    }
  }
  if (ri.i_inline & kDataExist) {
    vnode->SetFlag(InodeInfoFlag::kDataExist);
  }

  *out = std::move(vnode);
  return ZX_OK;
}

zx_status_t VnodeF2fs::OpenNode([[maybe_unused]] ValidatedOptions options,
                                fbl::RefPtr<Vnode> *out_redirect) {
  return ZX_OK;
}

zx_status_t VnodeF2fs::CloseNode() { return ZX_OK; }

void VnodeF2fs::RecycleNode() {
  {
    std::lock_guard lock(mutex_);
    ZX_ASSERT_MSG(open_count() == 0, "RecycleNode[%s:%u]: open_count must be zero (%lu)",
                  GetNameView().data(), GetKey(), open_count());
  }
  if (GetNlink()) {
    // Before freeing paged_vmo, ensure that all dirty pages are flushed.
    file_cache_.reset();
    vmo_manager_.reset();
    ReleasePagedVmo();
    // f2fs removes the last reference of |this| from the dirty vnode list
    // when there is no dirty Page for the vnode at checkpoint time. So, it
    // should not happen except when disk IOs fail.
    if (GetDirtyPageCount()) {
      // It can happen only when CpFlag::kCpErrorFlag is set or with tests.
      FX_LOGS(WARNING) << "Vnode[" << GetNameView().data() << ":" << GetKey()
                       << "] is deleted with " << GetDirtyPageCount() << " of dirty pages"
                       << ". CpFlag::kCpErrorFlag is "
                       << (fs()->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag)
                               ? "set."
                               : "not set.");
    }
    fs()->GetVCache().Downgrade(this);
  } else {
    // If PagedVfs::Teardown() releases the refptr of orphan vnodes, purge it at the next
    // mount time as f2fs object is not available anymore.
    if (!fs()->IsTearDown()) {
      EvictVnode();
    }
    file_cache_.reset();
    vmo_manager_.reset();
    ReleasePagedVmo();
    Deactivate();
    delete this;
  }
}

zx_status_t VnodeF2fs::GetAttributes(fs::VnodeAttributes *a) {
  *a = fs::VnodeAttributes();

  fs::SharedLock rlock(info_mutex_);
  a->mode = mode_;
  a->inode = ino_;
  a->content_size = size_;
  a->storage_size = GetBlockCount() * kBlockSize;
  a->link_count = nlink_;
  a->creation_time = zx_time_add_duration(ZX_SEC(ctime_.tv_sec), ctime_.tv_nsec);
  a->modification_time = zx_time_add_duration(ZX_SEC(mtime_.tv_sec), mtime_.tv_nsec);

  return ZX_OK;
}

zx_status_t VnodeF2fs::SetAttributes(fs::VnodeAttributesUpdate attr) {
  bool need_inode_sync = false;

  if (attr.has_creation_time()) {
    SetCTime(zx_timespec_from_duration(
        safemath::checked_cast<zx_duration_t>(attr.take_creation_time())));
    need_inode_sync = true;
  }
  if (attr.has_modification_time()) {
    SetMTime(zx_timespec_from_duration(
        safemath::checked_cast<zx_duration_t>(attr.take_modification_time())));
    need_inode_sync = true;
  }

  if (attr.any()) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (need_inode_sync) {
    MarkInodeDirty();
  }

  return ZX_OK;
}

struct f2fs_iget_args {
  uint64_t ino;
  int on_free;
};

#if 0  // porting needed
// void VnodeF2fs::F2fsSetInodeFlags() {
  // uint64_t &flags = fi.i_flags;

  // inode_.i_flags &= ~(S_SYNC | S_APPEND | S_IMMUTABLE |
  //     S_NOATIME | S_DIRSYNC);

  // if (flags & FS_SYNC_FL)
  //   inode_.i_flags |= S_SYNC;
  // if (flags & FS_APPEND_FL)
  //   inode_.i_flags |= S_APPEND;
  // if (flags & FS_IMMUTABLE_FL)
  //   inode_.i_flags |= S_IMMUTABLE;
  // if (flags & FS_NOATIME_FL)
  //   inode_.i_flags |= S_NOATIME;
  // if (flags & FS_DIRSYNC_FL)
  //   inode_.i_flags |= S_DIRSYNC;
// }

// int VnodeF2fs::F2fsIgetTest(void *data) {
  // f2fs_iget_args *args = (f2fs_iget_args *)data;

  // if (ino_ != args->ino)
  //   return 0;
  // if (i_state & (I_FREEING | I_WILL_FREE)) {
  //   args->on_free = 1;
  //   return 0;
  // }
  // return 1;
// }

// VnodeF2fs *VnodeF2fs::F2fsIgetNowait(uint64_t ino) {
//   fbl::RefPtr<VnodeF2fs> vnode_refptr;
//   VnodeF2fs *vnode = nullptr;
//   f2fs_iget_args args = {.ino = ino, .on_free = 0};
//   vnode = ilookup5(sb, ino, F2fsIgetTest, &args);

//   if (vnode)
//     return vnode;
//   if (!args.on_free) {
//     Vget(fs(), ino, &vnode_refptr);
//     vnode = vnode_refptr.get();
//     return vnode;
//   }
//   return static_cast<VnodeF2fs *>(ErrPtr(ZX_ERR_NOT_FOUND));
// }
#endif

zx_status_t VnodeF2fs::Vget(F2fs *fs, ino_t ino, fbl::RefPtr<VnodeF2fs> *out) {
  fbl::RefPtr<VnodeF2fs> vnode_refptr;

  if (fs->LookupVnode(ino, &vnode_refptr) == ZX_OK) {
    vnode_refptr->WaitForInit();
    *out = std::move(vnode_refptr);
    return ZX_OK;
  }

  if (zx_status_t status = Create(fs, ino, &vnode_refptr); status != ZX_OK) {
    return status;
  }

  if (ino != fs->GetSuperblockInfo().GetNodeIno() && ino != fs->GetSuperblockInfo().GetMetaIno()) {
    if (!fs->GetSuperblockInfo().IsOnRecovery() && vnode_refptr->GetNlink() == 0) {
      vnode_refptr->SetFlag(InodeInfoFlag::kBad);
      vnode_refptr.reset();
      *out = nullptr;
      return ZX_ERR_NOT_FOUND;
    }
  }

  if (zx_status_t status = fs->InsertVnode(vnode_refptr.get()); status != ZX_OK) {
    vnode_refptr->SetFlag(InodeInfoFlag::kBad);
    vnode_refptr.reset();
    if (fs->LookupVnode(ino, &vnode_refptr) == ZX_OK) {
      vnode_refptr->WaitForInit();
      *out = std::move(vnode_refptr);
      return ZX_OK;
    }
  }

  vnode_refptr->UnlockNewInode();
  *out = std::move(vnode_refptr);

  return ZX_OK;
}

void VnodeF2fs::UpdateInode(LockedPage &inode_page) {
  Node *rn;
  Inode *ri;

  inode_page->WaitOnWriteback();

  rn = inode_page->GetAddress<Node>();
  ri = &(rn->i);

  ri->i_mode = CpuToLe(GetMode());
  ri->i_advise = GetAdvise();
  ri->i_uid = CpuToLe(GetUid());
  ri->i_gid = CpuToLe(GetGid());
  ri->i_links = CpuToLe(GetNlink());
  ri->i_size = CpuToLe(GetSize());
  // For on-disk i_blocks, we keep counting inode block for backward compatibility.
  ri->i_blocks = CpuToLe(safemath::CheckAdd<uint64_t>(GetBlocks(), 1).ValueOrDie());
  SetRawExtent(ri->i_ext);

  ri->i_atime = CpuToLe(static_cast<uint64_t>(GetATime().tv_sec));
  ri->i_ctime = CpuToLe(static_cast<uint64_t>(GetCTime().tv_sec));
  ri->i_mtime = CpuToLe(static_cast<uint64_t>(GetMTime().tv_sec));
  ri->i_atime_nsec = CpuToLe(static_cast<uint32_t>(GetATime().tv_nsec));
  ri->i_ctime_nsec = CpuToLe(static_cast<uint32_t>(GetCTime().tv_nsec));
  ri->i_mtime_nsec = CpuToLe(static_cast<uint32_t>(GetMTime().tv_nsec));
  ri->i_current_depth = CpuToLe(static_cast<uint32_t>(GetCurDirDepth()));
  ri->i_xattr_nid = CpuToLe(GetXattrNid());
  ri->i_flags = CpuToLe(GetInodeFlags());
  ri->i_pino = CpuToLe(GetParentNid());
  ri->i_generation = CpuToLe(GetGeneration());
  ri->i_dir_level = GetDirLevel();

  std::string_view name = GetNameView();
  // double check |name|
  ZX_DEBUG_ASSERT(IsValidNameLength(name));
  auto size = safemath::checked_cast<uint32_t>(name.size());
  ri->i_namelen = CpuToLe(size);
  name.copy(reinterpret_cast<char *>(&ri->i_name[0]), size);

  if (TestFlag(InodeInfoFlag::kInlineData)) {
    ri->i_inline |= kInlineData;
  } else {
    ri->i_inline &= ~kInlineData;
  }
  if (TestFlag(InodeInfoFlag::kInlineDentry)) {
    ri->i_inline |= kInlineDentry;
  } else {
    ri->i_inline &= ~kInlineDentry;
  }
  if (GetExtraISize()) {
    ri->i_inline |= kExtraAttr;
    ri->i_extra_isize = GetExtraISize();
    if (TestFlag(InodeInfoFlag::kInlineXattr)) {
      ri->i_inline_xattr_size = GetInlineXattrAddrs();
    }
  }
  if (TestFlag(InodeInfoFlag::kDataExist)) {
    ri->i_inline |= kDataExist;
  } else {
    ri->i_inline &= ~kDataExist;
  }
  if (TestFlag(InodeInfoFlag::kInlineXattr)) {
    ri->i_inline |= kInlineXattr;
  } else {
    ri->i_inline &= ~kInlineXattr;
  }

  inode_page.SetDirty();
}

zx_status_t VnodeF2fs::WriteInode(bool is_reclaim) {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();
  zx_status_t ret = ZX_OK;

  if (ino_ == superblock_info.GetNodeIno() || ino_ == superblock_info.GetMetaIno()) {
    return ret;
  }

  if (IsDirty()) {
    fs::SharedLock rlock(superblock_info.GetFsLock(LockType::kNodeOp));
    LockedPage node_page;
    if (ret = fs()->GetNodeManager().GetNodePage(ino_, &node_page); ret != ZX_OK) {
      return ret;
    }
    UpdateInode(node_page);
  }

  return ZX_OK;
}

zx_status_t VnodeF2fs::DoTruncate(size_t len) {
  zx_status_t ret;

  if (ret = TruncateBlocks(len); ret == ZX_OK) {
    SetSize(len);
    if (GetSize() == 0) {
      ClearFlag(InodeInfoFlag::kDataExist);
    }

    timespec cur_time;
    clock_gettime(CLOCK_REALTIME, &cur_time);
    SetCTime(cur_time);
    SetMTime(cur_time);
    MarkInodeDirty();
  }

  fs()->GetSegmentManager().BalanceFs();
  return ret;
}

int VnodeF2fs::TruncateDataBlocksRange(NodePage &node_page, uint32_t ofs_in_node, uint32_t count) {
  int nr_free = 0;
  Node *raw_node = node_page.GetAddress<Node>();

  for (; count > 0; --count, ++ofs_in_node) {
    uint32_t *addr = BlkaddrInNode(*raw_node) + ofs_in_node;
    block_t blkaddr = LeToCpu(*addr);
    if (blkaddr == kNullAddr)
      continue;
    SetDataBlkaddr(node_page, ofs_in_node, kNullAddr);
    UpdateExtentCache(kNullAddr, node_page.StartBidxOfNode(*this) + ofs_in_node);
    fs()->GetSegmentManager().InvalidateBlocks(blkaddr);
    fs()->DecValidBlockCount(this, 1);
    ++nr_free;
  }
  if (nr_free) {
    LockedPage lock_page(fbl::RefPtr<Page>(&node_page), false);
    lock_page.SetDirty();
    lock_page.release(false);
    MarkInodeDirty();
  }
  return nr_free;
}

void VnodeF2fs::TruncateDataBlocks(NodePage &node_page) {
  TruncateDataBlocksRange(node_page, 0, kAddrsPerBlock);
}

void VnodeF2fs::TruncatePartialDataPage(uint64_t from) {
  const size_t offset_in_block = from % kBlockSize;
  if (!offset_in_block || TestFlag(InodeInfoFlag::kNoAlloc)) {
    return;
  }

  // Get page and zero the buffer after |from|.
  const pgoff_t block_index = safemath::CheckDiv(from, kBlockSize).ValueOrDie();
  LockedPage page;
  if (FindDataPage(block_index, &page) == ZX_OK) {
    page->WaitOnWriteback();
    if (auto dirty_or = page.SetVmoDirty(); dirty_or.is_error()) {
      VmoRead(safemath::CheckMul(page->GetKey(), kBlockSize).ValueOrDie(), kBlockSize);
      ZX_ASSERT(page.SetVmoDirty().is_ok());
    }
    page.Zero(offset_in_block, kPageSize);
    page.SetDirty();
  }
}

zx_status_t VnodeF2fs::TruncateBlocks(uint64_t from) {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();
  uint32_t blocksize = superblock_info.GetBlocksize();
  uint32_t count = 0;
  zx_status_t err;

  if (from > GetSize()) {
    return ZX_OK;
  }

  pgoff_t free_from =
      safemath::CheckRsh(fbl::round_up(from, blocksize), superblock_info.GetLogBlocksize())
          .ValueOrDie();
  {
    fs::SharedLock rlock(superblock_info.GetFsLock(LockType::kFileOp));
    do {
      LockedPage node_page;
      err = fs()->GetNodeManager().FindLockedDnodePage(*this, free_from, &node_page);
      if (err) {
        if (err == ZX_ERR_NOT_FOUND) {
          break;
        }
        return err;
      }

      if (IsInode(*node_page)) {
        count = GetAddrsPerInode();
      } else {
        count = kAddrsPerBlock;
      }

      uint32_t ofs_in_node;
      if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, free_from); result.is_error()) {
        return result.error_value();
      } else {
        ofs_in_node = result.value();
      }
      count -= ofs_in_node;
      ZX_ASSERT(count >= 0);

      if (ofs_in_node || IsInode(*node_page)) {
        TruncateDataBlocksRange(node_page.GetPage<NodePage>(), ofs_in_node, count);
      }
    } while (false);

    err = fs()->GetNodeManager().TruncateInodeBlocks(*this, free_from + count);
    // lastly zero out the first data page
    TruncatePartialDataPage(from);
  }
  // After nullifying addrs, invalidate the regarding pages
  InvalidatePages(free_from);
  return err;
}

zx_status_t VnodeF2fs::TruncateHole(pgoff_t pg_start, pgoff_t pg_end) {
  for (pgoff_t index = pg_start; index < pg_end; ++index) {
    LockedPage dnode_page;
    if (zx_status_t err = fs()->GetNodeManager().GetLockedDnodePage(*this, index, &dnode_page);
        err != ZX_OK) {
      if (err == ZX_ERR_NOT_FOUND) {
        continue;
      }
      return err;
    }

    uint32_t ofs_in_dnode;
    if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, index); result.is_error()) {
      if (result.error_value() == ZX_ERR_NOT_FOUND) {
        continue;
      }
      return result.error_value();
    } else {
      ofs_in_dnode = result.value();
    }

    if (DatablockAddr(&dnode_page.GetPage<NodePage>(), ofs_in_dnode) != kNullAddr) {
      TruncateDataBlocksRange(dnode_page.GetPage<NodePage>(), ofs_in_dnode, 1);
    }
  }
  InvalidatePages(pg_start, pg_end);
  return ZX_OK;
}

void VnodeF2fs::TruncateToSize() {
  if (!(IsDir() || IsReg() || IsLink()))
    return;

  if (zx_status_t ret = TruncateBlocks(GetSize()); ret == ZX_OK) {
    timespec cur_time;
    clock_gettime(CLOCK_REALTIME, &cur_time);
    SetMTime(cur_time);
    SetCTime(cur_time);
  }
}

void VnodeF2fs::ReleasePagedVmo() {
  std::lock_guard lock(mutex_);
  ReleasePagedVmoUnsafe();
}

void VnodeF2fs::ReleasePagedVmoUnsafe() {
  if (paged_vmo()) {
    fbl::RefPtr<fs::Vnode> pager_reference = FreePagedVmo();
    ZX_DEBUG_ASSERT(!pager_reference);
  }
}

void VnodeF2fs::EvictVnode() {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();

  if (ino_ == superblock_info.GetNodeIno() || ino_ == superblock_info.GetMetaIno()) {
    return;
  }

  if (GetNlink() || IsBad()) {
    return;
  }

  SetFlag(InodeInfoFlag::kNoAlloc);
  SetSize(0);
  if (HasBlocks()) {
    TruncateToSize();
  }

  {
    fs::SharedLock rlock(superblock_info.GetFsLock(LockType::kFileOp));
    fs()->GetNodeManager().RemoveInodePage(this);
  }
  fs()->EvictVnode(this);
}
void VnodeF2fs::InitFileCache() {
  if (IsReg()) {
    std::lock_guard lock(mutex_);
    if (auto size_or = CreatePagedVmo(); size_or.is_ok()) {
      zx_rights_t right = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHTS_PROPERTY | ZX_RIGHT_READ |
                          ZX_RIGHT_WRITE | ZX_RIGHT_RESIZE;
      zx::vmo vmo;
      ZX_ASSERT(paged_vmo().duplicate(right, &vmo) == ZX_OK);
      vmo_manager_ =
          std::make_unique<VmoManager>(VmoMode::kPaged, GetSize(), kVmoNodeSize, std::move(vmo));
    }
  } else {
    vmo_manager_ = std::make_unique<VmoManager>(VmoMode::kDiscardable, GetSize(), kVmoNodeSize);
  }
  file_cache_ = std::make_unique<FileCache>(this, vmo_manager_.get());
}
void VnodeF2fs::Init() {
  SetCurDirDepth(1);
  SetFlag(InodeInfoFlag::kInit);
  Activate();
}

void VnodeF2fs::MarkInodeDirty(bool to_back) {
  if (IsNode() || IsMeta() || !GetNlink()) {
    return;
  }
  if (!SetFlag(InodeInfoFlag::kDirty) || to_back) {
    ZX_ASSERT(fs()->GetVCache().AddDirty(this, to_back) == ZX_OK);
  }
}

void VnodeF2fs::Sync(SyncCallback closure) {
  closure(SyncFile(0, safemath::checked_cast<loff_t>(GetSize()), 0));
}

bool VnodeF2fs::NeedToCheckpoint() {
  if (!IsReg()) {
    return true;
  }
  if (GetNlink() != 1) {
    return true;
  }
  if (TestFlag(InodeInfoFlag::kNeedCp)) {
    return true;
  }
  if (!fs()->SpaceForRollForward()) {
    return true;
  }
  if (NeedToSyncDir()) {
    return true;
  }
  if (fs()->GetSuperblockInfo().TestOpt(kMountDisableRollForward)) {
    return true;
  }
  if (fs()->GetSuperblockInfo().FindVnodeFromVnodeSet(InoType::kModifiedDirIno, GetParentNid())) {
    return true;
  }
  return false;
}

zx_status_t VnodeF2fs::SyncFile(loff_t start, loff_t end, int datasync) {
  // When kCpErrorFlag is set, write is not allowed.
  if (fs()->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag)) {
    return ZX_ERR_BAD_STATE;
  }

  // TODO: When fdatasync is available, check if it should be written.
  if (!IsDirty()) {
    return ZX_OK;
  }

  // Write out dirty data pages and wait for completion
  WritebackOperation op = {.bSync = true};
  Writeback(op);

  // TODO: STRICT mode will be supported when FUA interface is added.
  // Currently, only POSIX mode is supported.
  // TODO: We should consider fdatasync for WriteInode().
  WriteInode(false);
  bool need_cp = NeedToCheckpoint();

  if (need_cp) {
    fs()->SyncFs();
    ClearFlag(InodeInfoFlag::kNeedCp);
    // Check if checkpoint errors happen during fsync().
    if (fs()->GetSuperblockInfo().TestCpFlags(CpFlag::kCpErrorFlag)) {
      return ZX_ERR_BAD_STATE;
    }
  } else {
    // Write dnode pages
    fs()->GetNodeManager().FsyncNodePages(*this);
    // TODO: Remove when FUA CMD is enabled
    fs()->GetBc().Flush();

    // TODO: Add flags to log recovery information to NAT entries and decide whether to write
    // inode or not.
  }
  return ZX_OK;
}

bool VnodeF2fs::NeedToSyncDir() const {
  ZX_ASSERT(GetParentNid() < kNullIno);
  return !fs()->GetNodeManager().IsCheckpointedNode(GetParentNid());
}

void VnodeF2fs::Notify(std::string_view name, fuchsia_io::wire::WatchEvent event) {
  watcher_.Notify(name, event);
}

zx_status_t VnodeF2fs::WatchDir(fs::Vfs *vfs, fuchsia_io::wire::WatchMask mask, uint32_t options,
                                fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) {
  return watcher_.WatchDir(vfs, this, mask, options, std::move(watcher));
}

void VnodeF2fs::GetExtentInfo(const Extent &extent) {
  std::lock_guard lock(extent_cache_mutex_);
  extent_cache_.fofs = LeToCpu(extent.fofs);
  extent_cache_.blk_addr = LeToCpu(extent.blk_addr);
  extent_cache_.len = LeToCpu(extent.len);
}

void VnodeF2fs::SetRawExtent(Extent &extent) {
  fs::SharedLock lock(extent_cache_mutex_);
  extent.fofs = CpuToLe(static_cast<uint32_t>(extent_cache_.fofs));
  extent.blk_addr = CpuToLe(extent_cache_.blk_addr);
  extent.len = CpuToLe(extent_cache_.len);
}

}  // namespace f2fs

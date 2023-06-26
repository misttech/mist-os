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
  std::lock_guard lock(mutex_);
  auto size_or = CreatePagedVmo(GetSize());
  if (size_or.is_error()) {
    return size_or.error_value();
  }
  return ClonePagedVmo(flags, *size_or, out_vmo);
}

zx::result<size_t> VnodeF2fs::CreatePagedVmo(size_t size) {
  if (!paged_vmo().is_valid()) {
    if (auto status = EnsureCreatePagedVmo(size, ZX_VMO_RESIZABLE | ZX_VMO_TRAP_DIRTY);
        status.is_error()) {
      return status.take_error();
    }
    SetPagedVmoName();
  }
  return zx::ok(size);
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

  uint32_t options = 0;
  if (flags & fuchsia_io::wire::VmoFlags::kPrivateClone) {
    options = ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE;
    // Allowed only on private vmo.
    rights |= ZX_RIGHT_SET_PROPERTY;
  } else {
    // |size| should be 0 with ZX_VMO_CHILD_REFERENCE.
    size = 0;
    options = ZX_VMO_CHILD_REFERENCE;
  }

  if (!(flags & fuchsia_io::wire::VmoFlags::kWrite)) {
    options |= ZX_VMO_CHILD_NO_WRITE;
  }

  zx::vmo vmo;
  if (auto status = paged_vmo().create_child(options, 0, size, &vmo); status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to duplicate VMO: " << zx_status_get_string(status);
    return status;
  }
  DidClonePagedVmo();

  if (auto status = vmo.replace(rights, &vmo); status != ZX_OK) {
    return status;
  }
  *out_vmo = std::move(vmo);
  return ZX_OK;
}

void VnodeF2fs::VmoRead(uint64_t offset, uint64_t length) {
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
    return ReportPagerErrorUnsafe(ZX_PAGER_VMO_READ, offset, length, size_or.error_value());
  }
  std::optional vfs = this->vfs();
  ZX_DEBUG_ASSERT(vfs.has_value());
  if (auto ret = vfs.value().get().SupplyPages(paged_vmo(), offset, *size_or, std::move(vmo), 0);
      ret.is_error()) {
    ReportPagerErrorUnsafe(ZX_PAGER_VMO_READ, offset, length, ret.error_value());
  }
}

block_t VnodeF2fs::GetReadBlockSize(block_t start_block, block_t req_size, block_t end_block) {
  block_t size = req_size;
  size = std::max(kDefaultReadaheadSize, size);
  if (start_block + size > end_block) {
    size = end_block - start_block;
  }
  return size;
}

zx::result<size_t> VnodeF2fs::CreateAndPopulateVmo(zx::vmo &vmo, const size_t offset,
                                                   const size_t length) {
  std::vector<bool> block_bitmap;
  size_t vmo_size = fbl::round_up(length, kBlockSize);
  size_t file_size = GetSize();
  block_t num_read_blocks = safemath::checked_cast<block_t>(vmo_size / kBlockSize);
  block_t start_block = safemath::checked_cast<block_t>(offset / kBlockSize);

  // Just supply pages for appended or inline area without read I/Os.
  if (!TestFlag(InodeInfoFlag::kInlineData) && !TestFlag(InodeInfoFlag::kNoAlloc) &&
      offset < file_size) {
    block_t end_block =
        safemath::checked_cast<block_t>(fbl::round_up(file_size, kBlockSize) / kBlockSize);
    num_read_blocks = GetReadBlockSize(start_block, num_read_blocks, end_block);
    // Get the information about which of blocks are subject to writeback.
    block_bitmap = file_cache_->GetDirtyPagesInfo(start_block, num_read_blocks);
    ZX_ASSERT(num_read_blocks == block_bitmap.size());
    vmo_size = num_read_blocks * kBlockSize;
  }

  // Create vmo to feed paged vmo.
  if (auto status = zx::vmo::create(vmo_size, 0, &vmo); status != ZX_OK) {
    return zx::error(status);
  }

  // Commit pages to |vmo|. Ensure that newly commited pages are set to zero.
  if (auto ret = vmo.op_range(ZX_VMO_OP_COMMIT, 0, vmo_size, nullptr, 0); ret != ZX_OK) {
    return zx::error(ret);
  }

  // Read blocks only for valid block addrs unless regarding pages are subject to writeback.
  if (block_bitmap.size()) {
    auto addrs =
        fs()->GetNodeManager().GetDataBlockAddresses(*this, start_block, num_read_blocks, true);
    if (addrs.is_error()) {
      return addrs.take_error();
    }
    int i = 0;
    block_t actual_read_blocks = 0;
    for (auto &addr : *addrs) {
      if (!block_bitmap[i++]) {
        // no read IOs for dirty and writeback pages.
        addr = kNullAddr;
      } else if (addr != kNewAddr && addr != kNullAddr) {
        // the number of blocks to be read.
        ++actual_read_blocks;
      }
    }
    if (actual_read_blocks) {
      if (auto status = fs()->MakeReadOperations(vmo, *addrs, PageType::kData); status.is_error()) {
        return status.take_error();
      }
    }
  }
  return zx::ok(vmo_size);
}

void VnodeF2fs::VmoDirty(uint64_t offset, uint64_t length) {
  fs::SharedLock lock(mutex_);
  std::optional vfs = this->vfs();
  ZX_DEBUG_ASSERT(vfs.has_value());
  auto ret = vfs.value().get().DirtyPages(paged_vmo(), offset, length);
  if (ret.is_error()) {
    // If someone has already dirtied or truncated these pages, do nothing.
    if (ret.error_value() == ZX_ERR_NOT_FOUND) {
      return;
    }
    ReportPagerErrorUnsafe(ZX_PAGER_OP_DIRTY, offset, length, ret.error_value());
  }
}

void VnodeF2fs::OnNoPagedVmoClones() {
  // Override PagedVnode::OnNoPagedVmoClones().
  // We intend to keep PagedVnode::paged_vmo alive while this vnode has any reference.
  ZX_DEBUG_ASSERT(!has_clones());
}

void VnodeF2fs::ReportPagerError(const uint32_t op, const uint64_t offset, const uint64_t length,
                                 const zx_status_t err) {
  fs::SharedLock lock(mutex_);
  return ReportPagerErrorUnsafe(op, offset, length, err);
}

void VnodeF2fs::ReportPagerErrorUnsafe(const uint32_t op, const uint64_t offset,
                                       const uint64_t length, const zx_status_t err) {
  std::optional vfs = this->vfs();
  ZX_DEBUG_ASSERT(vfs.has_value());
  zx_status_t pager_err = err;
  // Notifies the kernel that a page request for the given `range` has failed. Sent in response
  // to a `ZX_PAGER_VMO_READ` or `ZX_PAGER_VMO_DIRTY` page request. See `ZX_PAGER_OP_FAIL` for
  // more information.
  if (err != ZX_ERR_IO && err != ZX_ERR_IO_DATA_INTEGRITY && err != ZX_ERR_BAD_STATE &&
      err != ZX_ERR_NO_SPACE && err != ZX_ERR_BUFFER_TOO_SMALL) {
    pager_err = ZX_ERR_BAD_STATE;
  }
  FX_LOGS(WARNING) << "Failed to handle a pager request(" << std::hex << op << "). "
                   << zx_status_get_string(err);
  if (auto result = vfs.value().get().ReportPagerError(paged_vmo(), offset, length, pager_err);
      result.is_error()) {
    FX_LOGS(ERROR) << "Failed to report a pager error. " << result.status_string();
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

  Inode &inode = node_page->GetAddress<Node>()->i;
  fbl::RefPtr<VnodeF2fs> vnode;
  if (zx_status_t status = Allocate(fs, ino, LeToCpu(inode.i_mode), &vnode); status != ZX_OK) {
    return status;
  }

  vnode->SetUid(LeToCpu(inode.i_uid));
  vnode->SetGid(LeToCpu(inode.i_gid));
  vnode->SetNlink(LeToCpu(inode.i_links));
  // Don't count the in-memory inode.i_blocks for compatibility with the generic
  // filesystem including linux f2fs.
  vnode->SetBlocks(safemath::CheckSub<uint64_t>(LeToCpu(inode.i_blocks), 1).ValueOrDie());
  vnode->SetATime(LeToCpu(inode.i_atime), LeToCpu(inode.i_atime_nsec));
  vnode->SetCTime(LeToCpu(inode.i_ctime), LeToCpu(inode.i_ctime_nsec));
  vnode->SetMTime(LeToCpu(inode.i_mtime), LeToCpu(inode.i_mtime_nsec));
  vnode->SetGeneration(LeToCpu(inode.i_generation));
  vnode->SetParentNid(LeToCpu(inode.i_pino));
  vnode->SetCurDirDepth(LeToCpu(inode.i_current_depth));
  vnode->SetXattrNid(LeToCpu(inode.i_xattr_nid));
  vnode->SetInodeFlags(LeToCpu(inode.i_flags));
  vnode->SetDirLevel(inode.i_dir_level);
  vnode->UpdateVersion(LeToCpu(fs->GetSuperblockInfo().GetCheckpoint().checkpoint_ver) - 1);
  vnode->SetAdvise(inode.i_advise);
  vnode->GetExtentInfo(inode.i_ext);

  if (inode.i_inline & kInlineDentry) {
    vnode->SetFlag(InodeInfoFlag::kInlineDentry);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }
  if (inode.i_inline & kInlineData) {
    vnode->SetFlag(InodeInfoFlag::kInlineData);
  }
  if (inode.i_inline & kInlineXattr) {
    vnode->SetFlag(InodeInfoFlag::kInlineXattr);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }
  if (inode.i_inline & kExtraAttr) {
    vnode->SetExtraISize(LeToCpu(inode.i_extra_isize));
    if (inode.i_inline & kInlineXattr) {
      vnode->SetInlineXattrAddrs(LeToCpu(inode.i_inline_xattr_size));
    }
  }
  if (inode.i_inline & kDataExist) {
    vnode->SetFlag(InodeInfoFlag::kDataExist);
  }

  // If the roll-forward recovery creates it, it will initialize its cache from the latest inode
  // block later.
  if (!fs->GetSuperblockInfo().IsOnRecovery() || !vnode->GetNlink()) {
    vnode->InitFileCache(LeToCpu(inode.i_size));
  }

  std::string_view name(reinterpret_cast<char *>(inode.i_name),
                        std::min(kMaxNameLen, inode.i_namelen));
  if (inode.i_namelen != name.length() ||
      (ino != fs->GetSuperblockInfo().GetRootIno() && !fs::IsValidName(name))) {
    // TODO: Need to repair the file or set NeedFsck flag when fsck supports repair feature.
    // For now, we set kBad and clear link, so that it can be deleted without purging.
    vnode->ClearNlink();
    vnode->SetFlag(InodeInfoFlag::kBad);
    return ZX_ERR_NOT_FOUND;
  }
  vnode->SetName(name);

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
    file_cache_->Reset();
    fs()->GetVCache().Downgrade(this);
  } else {
    // If PagedVfs::Teardown() releases the refptr of orphan vnodes, purge it at the next
    // mount time as f2fs object is not available anymore.
    if (!fs()->IsTearDown()) {
      EvictVnode();
    }
    Deactivate();
    file_cache_->Reset();
    ReleasePagedVmo();
    delete this;
  }
}

zx_status_t VnodeF2fs::GetAttributes(fs::VnodeAttributes *a) {
  *a = fs::VnodeAttributes();

  fs::SharedLock rlock(info_mutex_);
  a->mode = mode_;
  a->inode = ino_;
  a->content_size = GetSize();
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
    SetDirty();
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
  fbl::RefPtr<VnodeF2fs> vnode;
  if (fs->LookupVnode(ino, &vnode) == ZX_OK) {
    vnode->WaitForInit();
    *out = std::move(vnode);
    return ZX_OK;
  }

  if (zx_status_t status = Create(fs, ino, &vnode); status != ZX_OK) {
    return status;
  }

  // VnodeCache should keep vnodes holding valid links except when purging orphans at mount time.
  if (ino != fs->GetSuperblockInfo().GetNodeIno() && ino != fs->GetSuperblockInfo().GetMetaIno() &&
      (!fs->GetSuperblockInfo().IsOnRecovery() && !vnode->GetNlink())) {
    vnode->SetFlag(InodeInfoFlag::kBad);
    return ZX_ERR_NOT_FOUND;
  }

  if (zx_status_t status = fs->InsertVnode(vnode.get()); status != ZX_OK) {
    ZX_ASSERT(status == ZX_ERR_ALREADY_EXISTS);
    vnode->SetFlag(InodeInfoFlag::kBad);
    vnode.reset();
    ZX_ASSERT(fs->LookupVnode(ino, &vnode) == ZX_OK);
    vnode->WaitForInit();
    *out = std::move(vnode);
    return ZX_OK;
  }

  vnode->UnlockNewInode();
  *out = std::move(vnode);
  return ZX_OK;
}

void VnodeF2fs::UpdateInodePage(LockedPage &inode_page) {
  inode_page->WaitOnWriteback();
  Inode &inode = inode_page->GetAddress<Node>()->i;

  inode.i_mode = CpuToLe(GetMode());
  inode.i_advise = GetAdvise();
  inode.i_uid = CpuToLe(GetUid());
  inode.i_gid = CpuToLe(GetGid());
  inode.i_links = CpuToLe(GetNlink());
  inode.i_size = CpuToLe(GetSize());
  // For on-disk i_blocks, we keep counting inode block for backward compatibility.
  inode.i_blocks = CpuToLe(safemath::CheckAdd<uint64_t>(GetBlocks(), 1).ValueOrDie());
  SetRawExtent(inode.i_ext);

  inode.i_atime = CpuToLe(static_cast<uint64_t>(GetATime().tv_sec));
  inode.i_ctime = CpuToLe(static_cast<uint64_t>(GetCTime().tv_sec));
  inode.i_mtime = CpuToLe(static_cast<uint64_t>(GetMTime().tv_sec));
  inode.i_atime_nsec = CpuToLe(static_cast<uint32_t>(GetATime().tv_nsec));
  inode.i_ctime_nsec = CpuToLe(static_cast<uint32_t>(GetCTime().tv_nsec));
  inode.i_mtime_nsec = CpuToLe(static_cast<uint32_t>(GetMTime().tv_nsec));
  inode.i_current_depth = CpuToLe(static_cast<uint32_t>(GetCurDirDepth()));
  inode.i_xattr_nid = CpuToLe(GetXattrNid());
  inode.i_flags = CpuToLe(GetInodeFlags());
  inode.i_pino = CpuToLe(GetParentNid());
  inode.i_generation = CpuToLe(GetGeneration());
  inode.i_dir_level = GetDirLevel();

  std::string_view name = GetNameView();
  // double check |name|
  ZX_DEBUG_ASSERT(IsValidNameLength(name));
  auto size = safemath::checked_cast<uint32_t>(name.size());
  inode.i_namelen = CpuToLe(size);
  name.copy(reinterpret_cast<char *>(&inode.i_name[0]), size);

  if (TestFlag(InodeInfoFlag::kInlineData)) {
    inode.i_inline |= kInlineData;
  } else {
    inode.i_inline &= ~kInlineData;
  }
  if (TestFlag(InodeInfoFlag::kInlineDentry)) {
    inode.i_inline |= kInlineDentry;
  } else {
    inode.i_inline &= ~kInlineDentry;
  }
  if (GetExtraISize()) {
    inode.i_inline |= kExtraAttr;
    inode.i_extra_isize = GetExtraISize();
    if (TestFlag(InodeInfoFlag::kInlineXattr)) {
      inode.i_inline_xattr_size = GetInlineXattrAddrs();
    }
  }
  if (TestFlag(InodeInfoFlag::kDataExist)) {
    inode.i_inline |= kDataExist;
  } else {
    inode.i_inline &= ~kDataExist;
  }
  if (TestFlag(InodeInfoFlag::kInlineXattr)) {
    inode.i_inline |= kInlineXattr;
  } else {
    inode.i_inline &= ~kInlineXattr;
  }

  inode_page.SetDirty();
}

zx_status_t VnodeF2fs::UpdateInodePage() {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();

  if (ino_ == superblock_info.GetNodeIno() || ino_ == superblock_info.GetMetaIno()) {
    return ZX_OK;
  }

  fs::SharedLock rlock(superblock_info.GetFsLock(LockType::kNodeOp));
  LockedPage node_page;
  if (zx_status_t ret = fs()->GetNodeManager().GetNodePage(ino_, &node_page); ret != ZX_OK) {
    return ret;
  }
  UpdateInodePage(node_page);
  return ZX_OK;
}

zx_status_t VnodeF2fs::DoTruncate(size_t len) {
  zx_status_t ret;

  if (ret = TruncateBlocks(len); ret == ZX_OK) {
    // SetSize() adjusts the size of its vmo or vmo content, and then the kernel guarantees
    // that its vmo after |len| are zeroed. If necessary, it triggers VmoDirty() to let f2fs write
    // changes to disk.
    SetSize(len);
    if (GetSize() == 0) {
      ClearFlag(InodeInfoFlag::kDataExist);
    }

    timespec cur_time;
    clock_gettime(CLOCK_REALTIME, &cur_time);
    SetCTime(cur_time);
    SetMTime(cur_time);
    SetDirty();
  }

  fs()->GetSegmentManager().BalanceFs();
  return ret;
}

int VnodeF2fs::TruncateDataBlocksRange(NodePage &node_page, uint32_t ofs_in_node, uint32_t count) {
  int nr_free = 0;
  for (; count > 0; --count, ++ofs_in_node) {
    block_t blkaddr = node_page.GetBlockAddr(ofs_in_node);
    if (blkaddr == kNullAddr) {
      continue;
    }
    SetDataBlkaddr(node_page, ofs_in_node, kNullAddr);
    UpdateExtentCache(kNullAddr, node_page.StartBidxOfNode(GetAddrsPerInode()) + ofs_in_node);
    fs()->GetSegmentManager().InvalidateBlocks(blkaddr);
    fs()->DecValidBlockCount(this, 1);
    ++nr_free;
  }
  if (nr_free) {
    LockedPage lock_page(fbl::RefPtr<Page>(&node_page), false);
    lock_page.SetDirty();
    lock_page.release(false);
    SetDirty();
  }
  return nr_free;
}

void VnodeF2fs::TruncateDataBlocks(NodePage &node_page) {
  TruncateDataBlocksRange(node_page, 0, kAddrsPerBlock);
}

zx_status_t VnodeF2fs::TruncateBlocks(uint64_t from) {
  SuperblockInfo &superblock_info = fs()->GetSuperblockInfo();
  uint32_t blocksize = superblock_info.GetBlocksize();

  if (from > GetSize()) {
    return ZX_OK;
  }

  pgoff_t free_from =
      safemath::CheckRsh(fbl::round_up(from, blocksize), superblock_info.GetLogBlocksize())
          .ValueOrDie();
  fs::SharedLock rlock(superblock_info.GetFsLock(LockType::kFileOp));
  // Invalidate data pages starting from |free_from|. It safely removes any pages updating their
  // block addrs before purging the addrs in nodes.
  InvalidatePages(free_from);
  {
    LockedPage node_page;
    if (zx_status_t err = fs()->GetNodeManager().FindLockedDnodePage(*this, free_from, &node_page);
        err == ZX_OK) {
      uint32_t ofs_in_node;
      if (auto result = fs()->GetNodeManager().GetOfsInDnode(*this, free_from); result.is_error()) {
        return result.error_value();
      } else {
        ofs_in_node = result.value();
      }

      // If |from| starts from inode or the middle of dnode, purge the addrs in the start dnode.
      NodePage &node = node_page.GetPage<NodePage>();
      if (ofs_in_node || node.IsInode()) {
        uint32_t count = 0;
        if (node.IsInode()) {
          count = safemath::CheckSub(GetAddrsPerInode(), ofs_in_node).ValueOrDie();
        } else {
          count = safemath::CheckSub(kAddrsPerBlock, ofs_in_node).ValueOrDie();
        }
        TruncateDataBlocksRange(node, ofs_in_node, count);
        free_from += count;
      }
    } else if (err != ZX_ERR_NOT_FOUND) {
      return err;
    }
  }

  // Invalidate the rest nodes.
  if (zx_status_t err = fs()->GetNodeManager().TruncateInodeBlocks(*this, free_from);
      err != ZX_OK) {
    return err;
  }

  return ZX_OK;
}

zx_status_t VnodeF2fs::TruncateHole(pgoff_t pg_start, pgoff_t pg_end) {
  InvalidatePages(pg_start, pg_end);
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

    if (dnode_page.GetPage<NodePage>().GetBlockAddr(ofs_in_dnode) != kNullAddr) {
      TruncateDataBlocksRange(dnode_page.GetPage<NodePage>(), ofs_in_dnode, 1);
    }
  }
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

void VnodeF2fs::InitFileCache(uint64_t nbytes) {
  zx::vmo vmo;
  VmoMode mode;
  size_t vmo_node_size = 0;

  ZX_DEBUG_ASSERT(!file_cache_);
  if (IsReg()) {
    std::lock_guard lock(mutex_);
    if (auto size_or = CreatePagedVmo(nbytes); size_or.is_ok()) {
      zx_rights_t right = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHTS_PROPERTY | ZX_RIGHT_READ |
                          ZX_RIGHT_WRITE | ZX_RIGHT_RESIZE;
      ZX_ASSERT(paged_vmo().duplicate(right, &vmo) == ZX_OK);
      mode = VmoMode::kPaged;
      vmo_node_size = zx_system_get_page_size();
    }
  } else {
    mode = VmoMode::kDiscardable;
    vmo_node_size = kVmoNodeSize;
  }
  ZX_ASSERT(!(zx_system_get_page_size() % kBlockSize));
  ZX_ASSERT(!(vmo_node_size % zx_system_get_page_size()));
  vmo_manager_ = std::make_unique<VmoManager>(mode, nbytes, vmo_node_size, std::move(vmo));
  file_cache_ = std::make_unique<FileCache>(this, vmo_manager_.get());
}

void VnodeF2fs::Init() {
  SetCurDirDepth(1);
  SetFlag(InodeInfoFlag::kInit);
  Activate();
}

bool VnodeF2fs::SetDirty() {
  if (IsNode() || IsMeta() || !HasLink()) {
    return false;
  }
  std::lock_guard lock(info_mutex_);
  if (fbl::DoublyLinkedListable<fbl::RefPtr<VnodeF2fs>>::InContainer()) {
    return false;
  }
  return fs()->GetVCache().AddDirty(this) == ZX_OK;
}

bool VnodeF2fs::ClearDirty() {
  std::lock_guard lock(info_mutex_);
  if (!fbl::DoublyLinkedListable<fbl::RefPtr<VnodeF2fs>>::InContainer()) {
    return false;
  }
  return fs()->GetVCache().RemoveDirty(this) == ZX_OK;
}

bool VnodeF2fs::IsDirty() {
  fs::SharedLock lock(info_mutex_);
  return fbl::DoublyLinkedListable<fbl::RefPtr<VnodeF2fs>>::InContainer();
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

void VnodeF2fs::SetSize(const size_t nbytes) {
  ZX_ASSERT(vmo_manager_);
  vmo_manager().SetContentSize(nbytes);
}

uint64_t VnodeF2fs::GetSize() const {
  ZX_ASSERT(vmo_manager_);
  return vmo_manager().GetContentSize();
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
  UpdateInodePage();
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

zx_status_t VnodeF2fs::WatchDir(fs::FuchsiaVfs *vfs, fuchsia_io::wire::WatchMask mask,
                                uint32_t options,
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

void VnodeF2fs::Activate() { SetFlag(InodeInfoFlag::kActive); }

void VnodeF2fs::Deactivate() {
  if (IsActive()) {
    ClearFlag(InodeInfoFlag::kActive);
    flag_cvar_.notify_all();
  }
}
void VnodeF2fs::WaitForDeactive(std::mutex &mutex) {
  if (IsActive()) {
    flag_cvar_.wait(mutex, [this]() { return !IsActive(); });
  }
}

bool VnodeF2fs::IsActive() const { return TestFlag(InodeInfoFlag::kActive); }

}  // namespace f2fs

// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/trace-provider/provider.h>
#include <lib/zx/event.h>
#include <sys/stat.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <safemath/checked_math.h>

namespace f2fs {

static zx_status_t CheckBlockSize(const Superblock& sb) {
  if (kF2fsSuperMagic != LeToCpu(sb.magic)) {
    return ZX_ERR_INVALID_ARGS;
  }

  block_t blocksize =
      safemath::CheckLsh<block_t>(1, LeToCpu(sb.log_blocksize)).ValueOrDefault(kUint32Max);
  if (blocksize != kPageSize)
    return ZX_ERR_INVALID_ARGS;
  // 512/1024/2048/4096 sector sizes are supported.
  if (LeToCpu(sb.log_sectorsize) > kMaxLogSectorSize ||
      LeToCpu(sb.log_sectorsize) < kMinLogSectorSize)
    return ZX_ERR_INVALID_ARGS;
  if ((LeToCpu(sb.log_sectors_per_block) + LeToCpu(sb.log_sectorsize)) != kMaxLogSectorSize)
    return ZX_ERR_INVALID_ARGS;
  return ZX_OK;
}

zx::result<std::unique_ptr<Superblock>> LoadSuperblock(BcacheMapper& bc) {
  BlockBuffer block;
  constexpr int kSuperblockCount = 2;
  zx_status_t status;
  for (auto i = 0; i < kSuperblockCount; ++i) {
    if (status = bc.Readblk(kSuperblockStart + i, block.get()); status != ZX_OK) {
      continue;
    }
    auto superblock = std::make_unique<Superblock>();
    std::memcpy(superblock.get(), block.get<uint8_t>() + kSuperOffset, sizeof(Superblock));
    if (status = CheckBlockSize(*superblock); status != ZX_OK) {
      continue;
    }
    return zx::ok(std::move(superblock));
  }
  FX_LOGS(ERROR) << "failed to read superblock." << status;
  return zx::error(status);
}

F2fs::F2fs(FuchsiaDispatcher dispatcher, std::unique_ptr<f2fs::BcacheMapper> bc,
           const MountOptions& mount_options, PlatformVfs* vfs)
    : dispatcher_(dispatcher), vfs_(vfs), bc_(std::move(bc)), mount_options_(mount_options) {
  inspect_tree_ = std::make_unique<InspectTree>(this);
  zx::event::create(0, &fs_id_);
}

void F2fs::StartMemoryPressureWatcher() {
  if (dispatcher_) {
    memory_pressure_watcher_ =
        std::make_unique<MemoryPressureWatcher>(dispatcher_, [this](MemoryPressure level) {
          MemoryPressure prev_level = current_memory_pressure_level_;
          // release-acquire ordering with HasNotEnoughMemory() and NeedToWriteback().
          current_memory_pressure_level_.store(level, std::memory_order_release);
          if (level > prev_level) {
            ScheduleWritebackAndReclaimPages();
          }
          FX_LOGS(INFO) << "Memory pressure level: " << MemoryPressureWatcher::ToString(level);
        });
  }
}

zx::result<std::unique_ptr<F2fs>> F2fs::Create(FuchsiaDispatcher dispatcher,
                                               std::unique_ptr<f2fs::BcacheMapper> bc,
                                               const MountOptions& options, PlatformVfs* vfs) {
  zx::result<std::unique_ptr<Superblock>> superblock_or;
  if (superblock_or = LoadSuperblock(*bc); superblock_or.is_error()) {
    return superblock_or.take_error();
  }

  auto fs = std::make_unique<F2fs>(dispatcher, std::move(bc), options, vfs);
  if (zx_status_t status = fs->LoadSuper(std::move(*superblock_or)); status != ZX_OK) {
    FX_LOGS(ERROR) << "failed to initialize fs." << status;
    return zx::error(status);
  }
  fs->StartMemoryPressureWatcher();
  return zx::ok(std::move(fs));
}

void F2fs::Sync(SyncCallback closure) {
  SyncFs(true);
  if (closure) {
    closure(ZX_OK);
  }
}

zx::result<fs::FilesystemInfo> F2fs::GetFilesystemInfo() {
  fs::FilesystemInfo info;

  info.block_size = kBlockSize;
  info.max_filename_size = kMaxNameLen;
  info.fs_type = fuchsia_fs::VfsType::kF2Fs;
  info.total_bytes =
      safemath::CheckMul<uint64_t>(superblock_info_->GetTotalBlockCount(), kBlockSize).ValueOrDie();
  info.used_bytes =
      safemath::CheckMul<uint64_t>(superblock_info_->GetValidBlockCount(), kBlockSize).ValueOrDie();
  info.total_nodes = superblock_info_->GetMaxNodeCount();
  info.used_nodes = superblock_info_->GetValidInodeCount();
  info.SetFsId(fs_id_);
  info.name = "f2fs";

  // TODO(unknown): Fill free_shared_pool_bytes using fvm info

  return zx::ok(info);
}

bool F2fs::IsValid() const {
  if (bc_ == nullptr) {
    return false;
  }
  if (root_vnode_ == nullptr) {
    return false;
  }
  if (superblock_info_ == nullptr) {
    return false;
  }
  if (segment_manager_ == nullptr) {
    return false;
  }
  if (node_manager_ == nullptr) {
    return false;
  }
  return true;
}

// Fill the locked page with data located in the block address.
zx::result<> F2fs::MakeReadOperation(LockedPage& page, block_t blk_addr, PageType type,
                                     bool is_sync) {
  if (page->IsUptodate()) {
    return zx::ok();
  }
  std::vector<block_t> addrs = {blk_addr};
  std::vector<LockedPage> pages;
  pages.push_back(std::move(page));
  auto status = MakeReadOperations(pages, addrs, type, is_sync);
  if (status.is_error()) {
    return status.take_error();
  }
  page = std::move(pages[0]);
  return zx::ok();
}

zx::result<> F2fs::MakeReadOperations(zx::vmo& vmo, std::vector<block_t>& addrs, PageType type,
                                      bool is_sync) {
  auto ret = reader_->ReadBlocks(vmo, addrs);
  if (ret.is_error()) {
    FX_LOGS(WARNING) << "failed to read blocks. " << ret.status_string();
    if (ret.status_value() == ZX_ERR_UNAVAILABLE || ret.status_value() == ZX_ERR_PEER_CLOSED) {
      // The underlying block device is unavailable. Set kCpErrorFlag for f2fs to enter read-only
      // mode.
      superblock_info_->SetCpFlags(CpFlag::kCpErrorFlag);
    }
  }
  return ret;
}

zx::result<> F2fs::MakeReadOperations(std::vector<LockedPage>& pages, std::vector<block_t>& addrs,
                                      PageType type, bool is_sync) {
  auto ret = reader_->ReadBlocks(pages, addrs);
  if (ret.is_error()) {
    FX_LOGS(WARNING) << "failed to read blocks. " << ret.status_string();
    if (ret.status_value() == ZX_ERR_UNAVAILABLE || ret.status_value() == ZX_ERR_PEER_CLOSED) {
      // The underlying block device is unavailable. Set kCpErrorFlag for f2fs to enter read-only
      // mode.
      superblock_info_->SetCpFlags(CpFlag::kCpErrorFlag);
    }
  }
  return ret;
}

zx_status_t F2fs::MakeTrimOperation(block_t blk_addr, block_t nblocks) const {
  return GetBc().Trim(blk_addr, nblocks);
}

void F2fs::PutSuper() {
#if 0  // porting needed
  // DestroyStats(superblock_info_.get());
  // StopGcThread(superblock_info_.get());
#endif

  if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
    // In the checkpoint error case, flush the dirty vnode list.
    GetVCache().ForDirtyVnodesIf([&](fbl::RefPtr<VnodeF2fs>& vnode) {
      ZX_ASSERT(vnode->ClearDirty());
      return ZX_OK;
    });
  }
  SetTearDown();
  writer_.reset();
  reader_.reset();
  GetVCache().Reset();
  Reset();
}

void F2fs::ScheduleWritebackAndReclaimPages(size_t num_pages) {
  // Schedule a Writer task for kernel to reclaim memory pages until the current memory pressure
  // becomes normal. If memory pressure events are not available, the task runs until the number of
  // dirty Pages is less than kMaxDirtyDataPages / 4. |writeback_flag_| ensures that neither
  // checkpoint nor gc runs during this writeback. If there is not enough space, stop writeback as
  // flushing N of dirty Pages can produce N of additional dirty node Pages in the worst case.
  if (HasNotEnoughMemory()) {
    auto promise = fpromise::make_promise([this]() __TA_EXCLUDES(writeback_mutex_) {
      std::lock_guard<std::shared_timed_mutex> lock(writeback_mutex_);
      while (!segment_manager_->HasNotEnoughFreeSecs() && CanReclaim() && HasNotEnoughMemory(4)) {
        GetVCache().ForDirtyVnodesIf(
            [&](fbl::RefPtr<VnodeF2fs>& vnode) {
              WritebackOperation op = {.bReclaim = true};
              vnode->Writeback(op);
              return ZX_OK;
            },
            [](fbl::RefPtr<VnodeF2fs>& vnode) {
              if (!vnode->IsDir() && vnode->GetDirtyPageCount()) {
                return ZX_OK;
              }
              return ZX_ERR_NEXT;
            });
      }
      node_vnode_->CleanupPages();
      GetVCache().EvictInactiveVnodes();
      return fpromise::ok();
    });
    writer_->ScheduleWriteback(std::move(promise));
  }
}

zx_status_t F2fs::SyncFs(bool bShutdown) {
  // TODO:: Consider !superblock_info_.IsDirty()
  // Stop writeback before checkpoint or gc
  FlagGuard flag(&stop_reclaim_flag_);
  ZX_ASSERT(flag);
  std::lock_guard<std::shared_timed_mutex> lock(writeback_mutex_);
  if (bShutdown) {
    FX_LOGS(INFO) << "prepare for shutdown";
    // Stop listening to memorypressure.
    memory_pressure_watcher_.reset();

    // Flush every dirty Pages.
    size_t target_vnodes = 0;
    do {
      // If CpFlag::kCpErrorFlag is set, it cannot be synchronized to disk. So we will drop all
      // dirty pages.
      if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
        return ZX_ERR_INTERNAL;
      }
      // If necessary, do gc.
      if (segment_manager_->HasNotEnoughFreeSecs()) {
        if (auto ret = StartGc(); ret.is_error()) {
          if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
            return ZX_ERR_INTERNAL;
          }
          // Run() returns ZX_ERR_UNAVAILABLE when there is no available victim section,
          // otherwise BUG
          ZX_DEBUG_ASSERT(ret.error_value() == ZX_ERR_UNAVAILABLE);
        }
      }
      target_vnodes = 0;
      WritebackOperation op = {.to_write = kDefaultBlocksPerSegment};
      op.if_vnode = [&target_vnodes](fbl::RefPtr<VnodeF2fs>& vnode) {
        if ((!vnode->IsDir() && vnode->GetDirtyPageCount()) || !vnode->IsValid()) {
          ++target_vnodes;
          return ZX_OK;
        }
        return ZX_ERR_NEXT;
      };
      fs::SharedLock lock(f2fs::GetGlobalLock());
      FlushDirtyDataPages(op);
    } while (superblock_info_->GetPageCount(CountType::kDirtyData) && target_vnodes);
  }
  return WriteCheckpoint(bShutdown);
}

#if 0  // porting needed
// int F2fs::F2fsStatfs(dentry *dentry /*, kstatfs *buf*/) {
  // super_block *sb = dentry->d_sb;
  // SuperblockInfo *superblock_info = F2FS_SB(sb);
  // u64 id = huge_encode_dev(sb->s_bdev->bd_dev);
  // block_t total_count, user_block_count, start_count, ovp_count;

  // total_count = LeToCpu(superblock_info->raw_super->block_count);
  // user_block_count = superblock_info->GetTotalBlockCount();
  // start_count = LeToCpu(superblock_info->raw_super->segment0_blkaddr);
  // ovp_count = GetSmInfo(superblock_info).ovp_segments << superblock_info->GetLogBlocksPerSeg();
  // buf->f_type = kF2fsSuperMagic;
  // buf->f_bsize = superblock_info->GetBlocksize();

  // buf->f_blocks = total_count - start_count;
  // buf->f_bfree = buf->f_blocks - .GetValidBlockCount(superblock_info) - ovp_count;
  // buf->f_bavail = user_block_count - .GetValidBlockCount(superblock_info);

  // buf->f_files = ValidInodeCount(superblock_info);
  // buf->f_ffree = superblock_info->GetTotalNodeCount() - ValidNodeCount(superblock_info);

  // buf->f_namelen = kMaxNameLen;
  // buf->f_fsid.val[0] = (u32)id;
  // buf->f_fsid.val[1] = (u32)(id >> 32);

  // return 0;
// }

// VnodeF2fs *F2fs::F2fsNfsGetInode(uint64_t ino, uint32_t generation) {
//   fbl::RefPtr<VnodeF2fs> vnode_refptr;
//   VnodeF2fs *vnode = nullptr;
//   int err;

//   if (ino < superblock_info_->GetRootIno())
//     return (VnodeF2fs *)ErrPtr(-ESTALE);

//   /*
//    * f2fs_iget isn't quite right if the inode is currently unallocated!
//    * However f2fs_iget currently does appropriate checks to handle stale
//    * inodes so everything is OK.
//    */
//   err = GetVnode(ino, &vnode_refptr);
//   if (err)
//     return (VnodeF2fs *)ErrPtr(err);
//   vnode = vnode_refptr.get();
//   if (generation && vnode->i_generation != generation) {
//     /* we didn't find the right inode.. */
//     return (VnodeF2fs *)ErrPtr(-ESTALE);
//   }
//   return vnode;
// }

// struct fid {};

// dentry *F2fs::F2fsFhToDentry(fid *fid, int fh_len, int fh_type) {
//   return generic_fh_to_dentry(sb, fid, fh_len, fh_type,
//             f2fs_nfs_get_inode);
// }

// dentry *F2fs::F2fsFhToParent(fid *fid, int fh_len, int fh_type) {
//   return generic_fh_to_parent(sb, fid, fh_len, fh_type,
//             f2fs_nfs_get_inode);
// }
#endif

void F2fs::Reset() {
  root_vnode_.reset();
  meta_vnode_.reset();
  node_vnode_.reset();
  node_manager_.reset();
  if (segment_manager_) {
    segment_manager_->DestroySegmentManager();
    segment_manager_.reset();
  }
  superblock_info_.reset();
}

zx_status_t F2fs::LoadSuper(std::unique_ptr<Superblock> sb) {
  auto reset = fit::defer([&] { Reset(); });

  // allocate memory for f2fs-specific super block info
  superblock_info_ = std::make_unique<SuperblockInfo>(std::move(sb), mount_options_);
  ClearOnRecovery();

  node_vnode_ = std::make_unique<VnodeF2fs>(this, superblock_info_->GetNodeIno(), 0);
  meta_vnode_ = std::make_unique<VnodeF2fs>(this, superblock_info_->GetMetaIno(), 0);

  reader_ = std::make_unique<Reader>(std::make_unique<StorageBufferPool>(bc_.get()));
  writer_ = std::make_unique<Writer>(std::make_unique<StorageBufferPool>(bc_.get()));
  if (zx_status_t err = GetValidCheckpoint(); err != ZX_OK) {
    return err;
  }

  segment_manager_ = std::make_unique<SegmentManager>(this);
  node_manager_ = std::make_unique<NodeManager>(this);
  if (zx_status_t err = segment_manager_->BuildSegmentManager(); err != ZX_OK) {
    return err;
  }

  if (zx_status_t err = node_manager_->BuildNodeManager(); err != ZX_OK) {
    return err;
  }

  // if there are nt orphan nodes free them
  if (zx_status_t err = PurgeOrphanInodes(); err != ZX_OK) {
    return err;
  }

  // read root inode and dentry
  zx::result vnode_or = GetVnode(superblock_info_->GetRootIno());
  if (vnode_or.is_error()) {
    return vnode_or.status_value();
  }
  root_vnode_ = std::move(*vnode_or);

  // root vnode is corrupted
  if (!root_vnode_->IsDir() || !root_vnode_->GetBlocks() || !root_vnode_->GetSize()) {
    return ZX_ERR_INTERNAL;
  }

  if (!superblock_info_->TestOpt(MountOption::kDisableRollForward)) {
    RecoverFsyncData();
  }

  // TODO(https://fxbug.dev/42070949): enable a thread for background gc
  // After POR, we can run background GC thread
  // err = StartGcThread(superblock_info);
  // if (err)
  //   goto fail;
  reset.cancel();
  return ZX_OK;
}

void F2fs::AddToVnodeSet(VnodeSet type, nid_t ino) {
  std::lock_guard lock(vnode_set_mutex_);
  uint32_t flag = 1 << static_cast<uint32_t>(type);
  auto& node = vnode_set_[ino];
  if (node & flag) {
    return;
  }
  node |= flag;
  ++vnode_set_size_[static_cast<size_t>(type)];
}
void F2fs::RemoveFromVnodeSet(VnodeSet type, nid_t ino) {
  std::lock_guard lock(vnode_set_mutex_);
  uint32_t flag = 1 << static_cast<uint32_t>(type);
  auto vnode = vnode_set_.find(ino);
  if (vnode == vnode_set_.end() || !(vnode->second & flag)) {
    return;
  }
  vnode->second &= ~flag;
  --vnode_set_size_[static_cast<uint32_t>(type)];
}
bool F2fs::FindVnodeSet(VnodeSet type, nid_t ino) {
  fs::SharedLock lock(vnode_set_mutex_);
  uint32_t flag = 1 << static_cast<uint32_t>(type);
  const auto vnode = vnode_set_.find(ino);
  if (vnode == vnode_set_.end()) {
    return false;
  }
  return vnode->second & flag;
}
size_t F2fs::GetVnodeSetSize(VnodeSet type) {
  fs::SharedLock lock(vnode_set_mutex_);
  return vnode_set_size_[static_cast<uint32_t>(type)];
}
void F2fs::ForAllVnodeSet(VnodeSet type, fit::function<void(nid_t)> callback) {
  fs::SharedLock lock(vnode_set_mutex_);
  uint32_t flag = 1 << static_cast<uint32_t>(type);
  for (const auto& nid : vnode_set_) {
    if (nid.second & flag) {
      callback(nid.first);
    }
  }
}
void F2fs::ClearVnodeSet() {
  std::lock_guard lock(vnode_set_mutex_);
  for (uint32_t i = 0; i < static_cast<uint32_t>(VnodeSet::kMax); ++i) {
    vnode_set_size_[i] = 0;
  }
  vnode_set_.clear();
}

zx::result<fbl::RefPtr<VnodeF2fs>> F2fs::GetVnode(ino_t ino) {
  if (ino < superblock_info_->GetRootIno() || !node_manager_->CheckNidRange(ino)) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  fbl::RefPtr<VnodeF2fs> vnode;
  if (vnode_cache_.Lookup(ino, &vnode) == ZX_OK) {
    if (unlikely(vnode->TestFlag(InodeInfoFlag::kNewInode))) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    return zx::ok(std::move(vnode));
  }

  LockedPage node_page;
  if (zx_status_t status = node_manager_->GetNodePage(ino, &node_page); status != ZX_OK) {
    return zx::error(status);
  }

  umode_t mode = LeToCpu(node_page->GetAddress<Node>()->i.i_mode);
  ZX_DEBUG_ASSERT(node_page.GetPage<NodePage>().InoOfNode() == ino);
  if (S_ISDIR(mode)) {
    vnode = fbl::MakeRefCounted<Dir>(this, ino, mode);
  } else {
    vnode = fbl::MakeRefCounted<File>(this, ino, mode);
  }
  vnode->Init(node_page);

  // VnodeCache is allowed to keep invalid vnodes only on recovery.
  if (!IsOnRecovery() && !vnode->GetNlink()) {
    vnode->SetFlag(InodeInfoFlag::kBad);
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  if (zx_status_t status = vnode_cache_.Add(vnode.get()); status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(vnode));
}

zx::result<fbl::RefPtr<VnodeF2fs>> F2fs::CreateNewVnode(umode_t mode, std::optional<gid_t> gid) {
  zx::result ino_or = node_manager_->AllocNid();
  if (ino_or.is_error()) {
    return ino_or.take_error();
  }

  fbl::RefPtr<VnodeF2fs> vnode;
  if (S_ISDIR(mode)) {
    vnode = fbl::MakeRefCounted<Dir>(this, *ino_or, mode);
  } else {
    vnode = fbl::MakeRefCounted<File>(this, *ino_or, mode);
  }

  vnode->SetFlag(InodeInfoFlag::kNewInode);
  if (gid) {
    vnode->SetGid(*gid);
    if (S_ISDIR(mode)) {
      vnode->SetMode(mode | S_ISGID);
    }
  } else {
    vnode->SetUid(getuid());
  }

  vnode->InitNlink();
  vnode->SetBlocks(0);
  vnode->InitTime();
  vnode->SetGeneration(superblock_info_->GetNextGeneration());
  superblock_info_->IncNextGeneration();

  if (superblock_info_->TestOpt(MountOption::kInlineXattr)) {
    vnode->SetFlag(InodeInfoFlag::kInlineXattr);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }

  if (superblock_info_->TestOpt(MountOption::kInlineDentry) && vnode->IsDir()) {
    vnode->SetFlag(InodeInfoFlag::kInlineDentry);
    vnode->SetInlineXattrAddrs(kInlineXattrAddrs);
  }

  if (vnode->IsReg()) {
    vnode->InitExtentTree();
  }
  vnode->InitFileCache();

  vnode_cache_.Add(vnode.get());
  vnode->SetDirty();

  return zx::ok(std::move(vnode));
}

zx::result<> F2fs::WaitForWriteback() {
  if (!writeback_mutex_.try_lock_shared_for(std::chrono::seconds(kWriteTimeOut))) {
    return zx::error(ZX_ERR_TIMED_OUT);
  }
  writeback_mutex_.unlock_shared();
  return zx::ok();
}

void F2fs::WaitForAvailableMemory() {
  while (HasNotEnoughMemory()) {
    ScheduleWritebackAndReclaimPages();
    ZX_ASSERT(WaitForWriteback().is_ok());
  }
}

// This function balances dirty node and dentry pages.
// In addition, it controls garbage collection.
void F2fs::BalanceFs(uint32_t num_blocks) {
  if (IsOnRecovery()) {
    return;
  }

  // If there is not enough memory, wait for writeback.
  WaitForAvailableMemory();
  if (segment_manager_->HasNotEnoughFreeSecs(0, num_blocks)) {
    // Stop writeback before gc. The writeback won't be invoked until gc acquires enough sections.
    FlagGuard flag(&stop_reclaim_flag_);
    ZX_ASSERT(flag);
    std::lock_guard<std::shared_timed_mutex> lock(writeback_mutex_);
    if (auto ret = StartGc(); ret.is_error()) {
      // Run() returns ZX_ERR_UNAVAILABLE when there is no available victim section, otherwise BUG
      ZX_DEBUG_ASSERT(ret.error_value() == ZX_ERR_UNAVAILABLE);
    }
  }
}

zx::result<uint32_t> F2fs::StartGc() {
  // For testinggc_type
  if (!segment_manager_->CanGc()) {
    return zx::ok(0);
  }

  if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  GcType gc_type = GcType::kBgGc;
  uint32_t sec_freed = 0;

  // FG_GC must run when there is no space (e.g., HasNotEnoughFreeSecs() == true).
  // If not, gc can compete with others (e.g., writeback) for victim Pages and space.
  while (segment_manager_->HasNotEnoughFreeSecs()) {
    if (superblock_info_->TestCpFlags(CpFlag::kCpErrorFlag)) {
      return zx::error(ZX_ERR_BAD_STATE);
    }
    // For example, if there are many prefree_segments below given threshold, we can make them
    // free by checkpoint. Then, we secure free segments which doesn't need fggc any more.
    if (segment_manager_->PrefreeSegments()) {
      auto before = segment_manager_->FreeSections();
      if (zx_status_t ret = WriteCheckpoint(false); ret != ZX_OK) {
        return zx::error(ret);
      }
      sec_freed =
          (safemath::CheckSub<uint32_t>(segment_manager_->FreeSections(), before) + sec_freed)
              .ValueOrDie();
      // After acquiring free sections, check if further gc is necessary.
      continue;
    }

    if (gc_type == GcType::kBgGc && segment_manager_->HasNotEnoughFreeSecs()) {
      gc_type = GcType::kFgGc;
    }

    auto segno_or = segment_manager_->GetGcVictim(gc_type, CursegType::kNoCheckType);
    if (segno_or.is_error()) {
      break;
    }
    if (auto err = segment_manager_->DoGarbageCollect(*segno_or, gc_type); err != ZX_OK) {
      return zx::error(err);
    }

    if (gc_type == GcType::kFgGc) {
      segment_manager_->SetCurVictimSec(kNullSecNo);
      if (zx_status_t ret = WriteCheckpoint(false); ret != ZX_OK) {
        return zx::error(ret);
      }
      ++sec_freed;
    }
  }
  if (!sec_freed) {
    return zx::error(ZX_ERR_UNAVAILABLE);
  }
  return zx::ok(sec_freed);
}

}  // namespace f2fs

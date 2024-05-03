// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/f2fs/f2fs.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/trace-provider/provider.h>
#include <lib/zx/event.h>
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
            ScheduleWriteback();
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
  if (gc_manager_ == nullptr) {
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
  pages.emplace_back(page.CopyRefPtr(), false);
  auto status = MakeReadOperations(pages, addrs, type, is_sync);
  [[maybe_unused]] auto locked_page = pages[0].release(false);
  if (status.is_error()) {
    return status.take_error();
  }
  return zx::ok();
}

zx::result<> F2fs::MakeReadOperations(zx::vmo& vmo, std::vector<block_t>& addrs, PageType type,
                                      bool is_sync) {
  auto ret = reader_->ReadBlocks(vmo, addrs);
  if (ret.is_error()) {
    FX_LOGS(WARNING) << "failed to read blocks. " << ret.status_string();
    if (ret.status_value() == ZX_ERR_UNAVAILABLE || ret.status_value() == ZX_ERR_PEER_CLOSED) {
      // It is not available when the block device is ZX_ERR_UNAVAILABLE or
      // ZX_ERR_PEER_CLOSED state. Set kCpErrorFlag to enter read-only mode.
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
      // It is not available when the block device is ZX_ERR_UNAVAILABLE or
      // ZX_ERR_PEER_CLOSED state. Set kCpErrorFlag to enter read-only mode.
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
  GetDirEntryCache().Reset();
  Reset();
}

void F2fs::ScheduleWriteback(size_t num_pages) {
  // Schedule a Writer task for kernel to reclaim memory pages until the current memory pressure
  // becomes normal. If memory pressure events are not available, the task runs until the number of
  // dirty Pages is less than kMaxDirtyDataPages / 4. |writeback_flag_| ensures that neither
  // checkpoint nor gc runs during this writeback. If there is not enough space, stop writeback as
  // flushing N of dirty Pages can produce N of additional dirty node Pages in the worst case.
  if (HasNotEnoughMemory() && writeback_flag_.try_acquire()) {
    auto promise = fpromise::make_promise([this]() mutable {
      while (!segment_manager_->HasNotEnoughFreeSecs() && CanReclaim() && !StopWriteback()) {
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
      // Wake waiters of WaitForWriteback().
      writeback_flag_.release();
      return fpromise::ok();
    });
    writer_->ScheduleWriteback(std::move(promise));
  }
}

zx_status_t F2fs::SyncFs(bool bShutdown) {
  // TODO:: Consider !superblock_info_.IsDirty()
  if (bShutdown) {
    FX_LOGS(INFO) << "prepare for shutdown";
    // Stop writeback before umount.
    FlagAcquireGuard flag(&stop_reclaim_flag_);
    ZX_DEBUG_ASSERT(flag.IsAcquired());
    ZX_ASSERT(WaitForWriteback().is_ok());
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
        if (auto ret = gc_manager_->Run(); ret.is_error()) {
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
  std::lock_guard lock(f2fs::GetGlobalLock());
  return WriteCheckpoint(false, bShutdown);
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
//   err = VnodeF2fs::Vget(this, ino, &vnode_refptr);
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
  gc_manager_.reset();
  superblock_info_.reset();
}

zx_status_t F2fs::LoadSuper(std::unique_ptr<Superblock> sb) {
  auto reset = fit::defer([&] { Reset(); });

  // allocate memory for f2fs-specific super block info
  superblock_info_ = std::make_unique<SuperblockInfo>(std::move(sb), mount_options_);
  ClearOnRecovery();

  node_vnode_ = std::make_unique<VnodeF2fs>(this, superblock_info_->GetNodeIno(), 0);
  meta_vnode_ = std::make_unique<VnodeF2fs>(this, superblock_info_->GetMetaIno(), 0);

  reader_ = std::make_unique<Reader>(bc_.get(), kDefaultBlocksPerSegment);
  writer_ = std::make_unique<Writer>(
      bc_.get(),
      safemath::CheckMul<size_t>(superblock_info_->GetActiveLogs(), kDefaultBlocksPerSegment)
          .ValueOrDie());

  if (zx_status_t err = GetValidCheckpoint(); err != ZX_OK) {
    return err;
  }

  segment_manager_ = std::make_unique<SegmentManager>(this);
  node_manager_ = std::make_unique<NodeManager>(this);
  gc_manager_ = std::make_unique<GcManager>(this);
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
  if (zx_status_t err = VnodeF2fs::Vget(this, superblock_info_->GetRootIno(), &root_vnode_);
      err != ZX_OK) {
    return err;
  }

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

}  // namespace f2fs

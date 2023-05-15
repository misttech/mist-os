// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_VNODE_H_
#define SRC_STORAGE_F2FS_VNODE_H_

#include "src/storage/f2fs/file_cache.h"
#include "src/storage/f2fs/vmo_manager.h"

namespace f2fs {
constexpr uint32_t kNullIno = std::numeric_limits<uint32_t>::max();

class F2fs;
// for in-memory extent cache entry
struct ExtentInfo {
  uint64_t fofs = 0;      // start offset in a file
  uint32_t blk_addr = 0;  // start block address of the extent
  uint32_t len = 0;       // length of the extent
};

// i_advise uses Fadvise:xxx bit. We can add additional hints later.
enum class FAdvise {
  kCold = 1,
};

struct LockedPagesAndAddrs {
  std::vector<block_t> block_addrs;  // Allocated block address
  std::vector<LockedPage> pages;     // Pages matched with block address
};

class VnodeF2fs : public fs::PagedVnode,
                  public fbl::Recyclable<VnodeF2fs>,
                  public fbl::WAVLTreeContainable<VnodeF2fs *>,
                  public fbl::DoublyLinkedListable<fbl::RefPtr<VnodeF2fs>> {
 public:
  explicit VnodeF2fs(F2fs *fs, ino_t ino, umode_t mode);
  ~VnodeF2fs() { ReleasePagedVmo(); }

  uint32_t InlineDataOffset() const {
    return kPageSize - sizeof(NodeFooter) -
           sizeof(uint32_t) * (kAddrsPerInode + kNidsPerInode - 1) + GetExtraISize();
  }
  uint32_t MaxInlineData() const {
    return safemath::CheckMul<uint32_t>(sizeof(uint32_t), (GetAddrsPerInode() - 1)).ValueOrDie();
  }
  uint32_t MaxInlineDentry() const {
    return safemath::checked_cast<uint32_t>(
        safemath::CheckDiv(safemath::CheckMul(MaxInlineData(), kBitsPerByte).ValueOrDie(),
                           ((kSizeOfDirEntry + kDentrySlotLen) * kBitsPerByte + 1))
            .ValueOrDie());
  }
  uint32_t GetAddrsPerInode() const {
    return safemath::checked_cast<uint32_t>(
        (safemath::CheckSub(kAddrsPerInode, safemath::CheckDiv(GetExtraISize(), sizeof(uint32_t))) -
         GetInlineXattrAddrs())
            .ValueOrDie());
  }

  static zx_status_t Allocate(F2fs *fs, ino_t ino, umode_t mode, fbl::RefPtr<VnodeF2fs> *out);
  static zx_status_t Create(F2fs *fs, ino_t ino, fbl::RefPtr<VnodeF2fs> *out);
  static zx_status_t Vget(F2fs *fs, ino_t ino, fbl::RefPtr<VnodeF2fs> *out);

  void Init();
  void InitFileCache(uint64_t nbytes = 0) __TA_EXCLUDES(mutex_);

  ino_t GetKey() const { return ino_; }

  void Sync(SyncCallback closure) override;
  zx_status_t SyncFile(loff_t start, loff_t end, int datasync);

  void fbl_recycle() { RecycleNode(); }

  F2fs *fs() const { return fs_; }

  ino_t Ino() const { return ino_; }

  zx_status_t GetAttributes(fs::VnodeAttributes *a) final __TA_EXCLUDES(info_mutex_);
  zx_status_t SetAttributes(fs::VnodeAttributesUpdate attr) final __TA_EXCLUDES(info_mutex_);

  fs::VnodeProtocolSet GetProtocols() const override;

  zx_status_t GetNodeInfoForProtocol([[maybe_unused]] fs::VnodeProtocol protocol,
                                     [[maybe_unused]] fs::Rights rights,
                                     fs::VnodeRepresentation *info) override;

  // For fs::PagedVnode
  zx_status_t GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo *out_vmo) override
      __TA_EXCLUDES(mutex_);
  void VmoRead(uint64_t offset, uint64_t length) override __TA_EXCLUDES(mutex_);
  void VmoDirty(uint64_t offset, uint64_t length) override __TA_EXCLUDES(mutex_);

  zx::result<size_t> CreateAndPopulateVmo(zx::vmo &vmo, const size_t offset, const size_t length)
      __TA_EXCLUDES(mutex_);
  void OnNoPagedVmoClones() final __TA_REQUIRES(mutex_);

  void ReleasePagedVmoUnsafe() __TA_REQUIRES(mutex_);
  void ReleasePagedVmo() __TA_EXCLUDES(mutex_);

#if 0  // porting needed
  // void F2fsSetInodeFlags();
  // int F2fsIgetTest(void *data);
  // static int CheckExtentCache(inode *inode, pgoff_t pgofs,
  // static int GetDataBlockRo(inode *inode, sector_t iblock,
  //      buffer_head *bh_result, int create);
#endif

  void UpdateInode(LockedPage &inode_page);
  zx_status_t WriteInode(bool is_reclaim = false);
  zx_status_t DoTruncate(size_t len);
  // Caller should ensure node_page is locked.
  int TruncateDataBlocksRange(NodePage &node_page, uint32_t ofs_in_node, uint32_t count);
  // Caller should ensure node_page is locked.
  void TruncateDataBlocks(NodePage &node_page);
  zx_status_t TruncateBlocks(uint64_t from);
  zx_status_t TruncateHole(pgoff_t pg_start, pgoff_t pg_end);
  void TruncateToSize();
  void EvictVnode();

  // Caller should ensure node_page is locked.
  void SetDataBlkaddr(NodePage &node_page, uint32_t ofs_in_node, block_t new_addr);
  zx::result<block_t> FindDataBlkAddr(pgoff_t index);
  // Caller should ensure node_page is locked.
  zx_status_t ReserveNewBlock(NodePage &node_page, uint32_t ofs_in_node);

  void UpdateExtentCache(block_t blk_addr, pgoff_t file_offset);
  zx::result<LockedPage> FindDataPage(pgoff_t index, bool do_read = true);
  // This function returns block addresses and LockedPages for requested offsets. If there is no
  // node page of a offset or the block address is not assigned, this function adds null LockedPage
  // and kNullAddr to LockedPagesAndAddrs struct. The handling of null LockedPage and kNullAddr is
  // responsible for StorageBuffer::ReserveReadOperations().
  zx::result<LockedPagesAndAddrs> FindDataBlockAddrsAndPages(const pgoff_t start,
                                                             const pgoff_t end);
  zx_status_t GetLockedDataPage(pgoff_t index, LockedPage *out);
  zx::result<std::vector<LockedPage>> GetLockedDataPages(pgoff_t start, pgoff_t end);
  zx_status_t GetNewDataPage(pgoff_t index, bool new_i_size, LockedPage *out);

  zx::result<block_t> GetBlockAddrForDirtyDataPage(LockedPage &page, bool is_reclaim);
  zx::result<std::vector<LockedPage>> WriteBegin(const size_t offset, const size_t len);

  virtual zx_status_t RecoverInlineData(NodePage &node_page) __TA_EXCLUDES(info_mutex_) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  void Notify(std::string_view name, fuchsia_io::wire::WatchEvent event) final;
  zx_status_t WatchDir(fs::FuchsiaVfs *vfs, fuchsia_io::wire::WatchMask mask, uint32_t options,
                       fidl::ServerEnd<fuchsia_io::DirectoryWatcher> watcher) final;

  // Set dirty flag and insert |this| to VnodeCache::dirty_list_.
  // If |to_back| is set and |this| is already in the list, it moves |this| to the back of the list.
  void MarkInodeDirty(bool to_back = false);

  void GetExtentInfo(const Extent &i_ext) __TA_EXCLUDES(extent_cache_mutex_);
  void SetRawExtent(Extent &i_ext) __TA_EXCLUDES(extent_cache_mutex_);

  void InitNlink() { nlink_.store(1, std::memory_order_release); }
  void IncNlink() { nlink_.fetch_add(1); }
  void DropNlink() { nlink_.fetch_sub(1); }
  void ClearNlink() { nlink_.store(0, std::memory_order_release); }
  void SetNlink(const uint32_t nlink) { nlink_.store(nlink, std::memory_order_release); }
  uint32_t GetNlink() const { return nlink_.load(std::memory_order_acquire); }

  void SetMode(const umode_t &mode);
  umode_t GetMode() const;
  bool IsDir() const;
  bool IsReg() const;
  bool IsLink() const;
  bool IsChr() const;
  bool IsBlk() const;
  bool IsSock() const;
  bool IsFifo() const;
  bool HasGid() const;
  bool IsMeta() const;
  bool IsNode() const;

  void SetName(std::string_view name) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    name_ = name;
  }
  bool IsSameName(std::string_view name) __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return (name_.GetStringView().compare(name) == 0);
  }
  std::string_view GetNameView() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return name_.GetStringView();
  }

  // stat_lock
  uint64_t GetBlockCount() const {
    return safemath::CheckDiv<uint64_t>(fbl::round_up(GetSize(), kBlockSize), kBlockSize)
        .ValueOrDie();
  }
  void IncBlocks(const block_t &nblocks) { num_blocks_.fetch_add(nblocks); }
  void DecBlocks(const block_t &nblocks) {
    ZX_ASSERT(num_blocks_ >= nblocks);
    num_blocks_.fetch_sub(nblocks, std::memory_order_release);
  }
  void InitBlocks() { SetBlocks(0); }
  block_t GetBlocks() const { return num_blocks_.load(std::memory_order_acquire); }
  void SetBlocks(const uint64_t &blocks) {
    num_blocks_.store(safemath::checked_cast<block_t>(blocks), std::memory_order_release);
  }
  bool HasBlocks() const {
    block_t xattr_block = GetXattrNid() ? 1 : 0;
    return (GetBlocks() > xattr_block);
  }

  void SetSize(const size_t nbytes);
  uint64_t GetSize() const;

  void SetParentNid(const ino_t &pino) { parent_ino_.store(pino, std::memory_order_release); }
  ino_t GetParentNid() const { return parent_ino_.load(std::memory_order_acquire); }

  void IncreaseDirtyPageCount() { dirty_pages_.fetch_add(1); }
  void DecreaseDirtyPageCount() { dirty_pages_.fetch_sub(1); }
  block_t GetDirtyPageCount() const { return dirty_pages_.load(std::memory_order_acquire); }

  void SetGeneration(const uint32_t &gen) { generation_ = gen; }
  uint32_t GetGeneration() const { return generation_; }

  void SetUid(const uid_t &uid) { uid_ = uid; }
  uid_t GetUid() const { return uid_; }

  void SetGid(const gid_t &gid) { gid_ = gid; }
  gid_t GetGid() const { return gid_; }

  timespec GetATime() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return atime_;
  }
  void SetATime(const timespec &time) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    atime_ = time;
  }
  void SetATime(const uint64_t &sec, const uint32_t &nsec) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    atime_.tv_sec = sec;
    atime_.tv_nsec = nsec;
  }
  timespec GetMTime() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return mtime_;
  }
  void SetMTime(const timespec &time) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    mtime_ = time;
  }
  void SetMTime(const uint64_t &sec, const uint32_t &nsec) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    mtime_.tv_sec = sec;
    mtime_.tv_nsec = nsec;
  }
  timespec GetCTime() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return ctime_;
  }
  void SetCTime(const timespec &time) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    ctime_ = time;
  }
  void SetCTime(const uint64_t &sec, const uint32_t &nsec) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    ctime_.tv_sec = sec;
    ctime_.tv_nsec = nsec;
  }

  void SetInodeFlags(const uint32_t flags) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    inode_flags_ = flags;
  }
  uint32_t GetInodeFlags() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return inode_flags_;
  }

  void ClearAdvise(const FAdvise bit) __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    ClearBit(static_cast<int>(bit), &advise_);
  }
  void SetAdvise(const FAdvise bit) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    SetBit(static_cast<int>(bit), &advise_);
  }
  uint8_t GetAdvise() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return advise_;
  }
  void SetAdvise(const uint8_t bits) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    advise_ = bits;
  }
  int IsAdviseSet(const FAdvise bit) __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return TestBit(static_cast<int>(bit), &advise_);
  }

  uint64_t GetDirHashLevel() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return clevel_;
  }
  bool IsSameDirHash(const f2fs_hash_t hash) __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return (chash_ == hash);
  }
  void ClearDirHash() __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    chash_ = 0;
  }
  void SetDirHash(const f2fs_hash_t hash, const uint64_t &level) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    chash_ = hash;
    clevel_ = level;
  }
  uint64_t GetCurDirDepth() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return current_depth_;
  }
  void SetCurDirDepth(const uint64_t depth) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    current_depth_ = depth;
  }

  uint8_t GetDirLevel() __TA_EXCLUDES(info_mutex_) {
    fs::SharedLock lock(info_mutex_);
    return dir_level_;
  }
  void SetDirLevel(const uint8_t level) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    dir_level_ = level;
  }
  void UpdateVersion(const uint64_t version) __TA_EXCLUDES(info_mutex_) {
    std::lock_guard lock(info_mutex_);
    data_version_ = version;
  }

  nid_t GetXattrNid() const { return xattr_nid_; }
  void SetXattrNid(const nid_t nid) { xattr_nid_ = nid; }
  void ClearXattrNid() { xattr_nid_ = 0; }
  uint16_t GetInlineXattrAddrs() const { return inline_xattr_size_; }
  void SetInlineXattrAddrs(const uint16_t addrs) { inline_xattr_size_ = addrs; }

  uint16_t GetExtraISize() const { return extra_isize_; }
  void SetExtraISize(const uint16_t size) { extra_isize_ = size; }

  // Release-acquire ordering for Set/ClearFlag and TestFlag
  bool SetFlag(const InodeInfoFlag &flag) {
    return flags_[static_cast<uint8_t>(flag)].test_and_set(std::memory_order_release);
  }
  void ClearFlag(const InodeInfoFlag &flag) {
    flags_[static_cast<uint8_t>(flag)].clear(std::memory_order_release);
  }
  bool TestFlag(const InodeInfoFlag &flag) const {
    return flags_[static_cast<uint8_t>(flag)].test(std::memory_order_acquire);
  }
  void WaitOnFlag(InodeInfoFlag flag) const {
    while (flags_[static_cast<uint8_t>(flag)].test(std::memory_order_acquire)) {
      flags_[static_cast<uint8_t>(flag)].wait(true, std::memory_order_relaxed);
    }
  }

  void Activate();
  void Deactivate();
  bool IsActive() const;
  void WaitForDeactive(std::mutex &mutex);

  void ClearDirty() { ClearFlag(InodeInfoFlag::kDirty); }
  bool IsDirty() const { return TestFlag(InodeInfoFlag::kDirty); }
  bool IsBad() const { return TestFlag(InodeInfoFlag::kBad); }

  void WaitForInit() const { WaitOnFlag(InodeInfoFlag::kInit); }
  void UnlockNewInode() {
    ClearFlag(InodeInfoFlag::kInit);
    flags_[static_cast<uint8_t>(InodeInfoFlag::kInit)].notify_all();
  }

  bool ShouldFlush() const { return !(!GetNlink() || !IsDirty() || IsBad()); }

  zx_status_t FindPage(pgoff_t index, fbl::RefPtr<Page> *out) {
    return file_cache_->FindPage(index, out);
  }

  zx::result<std::vector<LockedPage>> FindPages(pgoff_t start, pgoff_t end) {
    return file_cache_->FindPages(start, end);
  }

  zx_status_t GrabCachePage(pgoff_t index, LockedPage *out) {
    return file_cache_->GetPage(index, out);
  }

  zx::result<std::vector<LockedPage>> GrabCachePages(pgoff_t start, pgoff_t end) {
    return file_cache_->GetPages(start, end);
  }

  zx::result<std::vector<LockedPage>> GrabCachePages(const std::vector<pgoff_t> &page_offsets) {
    return file_cache_->GetPages(page_offsets);
  }

  pgoff_t Writeback(WritebackOperation &operation) { return file_cache_->Writeback(operation); }

  std::vector<LockedPage> InvalidatePages(pgoff_t start = 0, pgoff_t end = kPgOffMax) {
    return file_cache_->InvalidatePages(start, end);
  }
  void ResetFileCache(pgoff_t start = 0, pgoff_t end = kPgOffMax) { file_cache_->Reset(); }
  void ClearDirtyPagesForOrphan() {
    if (!file_cache_->SetOrphan()) {
      file_cache_->ClearDirtyPages();
    }
  }

  PageType GetPageType() {
    if (IsNode()) {
      return PageType::kNode;
    } else if (IsMeta()) {
      return PageType::kMeta;
    } else {
      return PageType::kData;
    }
  }

  // For testing
  bool HasPagedVmo() __TA_EXCLUDES(mutex_) {
    fs::SharedLock rlock(mutex_);
    return paged_vmo().is_valid();
  }

  // Overriden methods for thread safety analysis annotations.
  zx_status_t Read(void *data, size_t len, size_t off, size_t *out_actual) override
      __TA_EXCLUDES(mutex_) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t Write(const void *data, size_t len, size_t offset, size_t *out_actual) override
      __TA_EXCLUDES(mutex_) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t Append(const void *data, size_t len, size_t *out_end, size_t *out_actual) override
      __TA_EXCLUDES(mutex_) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  zx_status_t Truncate(size_t len) override __TA_EXCLUDES(mutex_) { return ZX_ERR_NOT_SUPPORTED; }
  DirtyPageList &GetDirtyPageList() const { return file_cache_->GetDirtyPageList(); }
  VmoManager &GetVmoManager() const { return vmo_manager(); }

  block_t GetReadBlockSize(block_t start_block, block_t req_size, block_t end_block);

 protected:
  void RecycleNode() override;
  VmoManager &vmo_manager() const { return *vmo_manager_; }
  void ReportPagerError(const uint32_t op, const uint64_t offset, const uint64_t length,
                        const zx_status_t err) __TA_EXCLUDES(mutex_);
  void ReportPagerErrorUnsafe(const uint32_t op, const uint64_t offset, const uint64_t length,
                              const zx_status_t err) __TA_REQUIRES_SHARED(mutex_);

 private:
  zx::result<block_t> GetBlockAddrForDataPage(LockedPage &page);
  zx_status_t OpenNode(ValidatedOptions options, fbl::RefPtr<Vnode> *out_redirect) final
      __TA_EXCLUDES(mutex_);
  zx_status_t CloseNode() final;

  bool NeedToSyncDir() const;
  bool NeedToCheckpoint();

  zx::result<size_t> CreatePagedVmo(uint64_t size) __TA_REQUIRES(mutex_);
  zx_status_t ClonePagedVmo(fuchsia_io::wire::VmoFlags flags, size_t size, zx::vmo *out_vmo)
      __TA_REQUIRES(mutex_);
  void SetPagedVmoName() __TA_REQUIRES(mutex_);

  std::unique_ptr<VmoManager> vmo_manager_;
  std::unique_ptr<FileCache> file_cache_;

  uid_t uid_ = 0;
  gid_t gid_ = 0;
  uint32_t generation_ = 0;
  std::atomic<block_t> num_blocks_ = 0;
  std::atomic<uint32_t> nlink_ = 0;
  std::atomic<block_t> dirty_pages_ = 0;  // # of dirty dentry/data pages
  std::atomic<ino_t> parent_ino_ = kNullIno;
  std::array<std::atomic_flag, static_cast<uint8_t>(InodeInfoFlag::kFlagSize)> flags_ = {
      ATOMIC_FLAG_INIT};
  std::condition_variable_any flag_cvar_;

  fs::SharedMutex info_mutex_;
  uint64_t current_depth_ __TA_GUARDED(info_mutex_) = 0;  // use only in directory structure
  uint64_t data_version_ __TA_GUARDED(info_mutex_) = 0;   // lastest version of data for fsync
  uint64_t clevel_ __TA_GUARDED(info_mutex_) = 0;         // maximum level of given file name
  uint32_t inode_flags_ __TA_GUARDED(info_mutex_) = 0;    // keep an inode flags for ioctl
  uint16_t extra_isize_ = 0;                              // extra inode attribute size in bytes
  uint16_t inline_xattr_size_ = 0;                        // inline xattr size
  [[maybe_unused]] umode_t acl_mode_ = 0;                 // keep file acl mode temporarily
  uint8_t advise_ __TA_GUARDED(info_mutex_) = 0;          // use to give file attribute hints
  uint8_t dir_level_ __TA_GUARDED(info_mutex_) = 0;       // use for dentry level for large dir
  f2fs_hash_t chash_ __TA_GUARDED(info_mutex_);           // hash value of given file name
  // TODO: revisit thread annotation when xattr is available.
  nid_t xattr_nid_ = 0;  // node id that contains xattrs
  NameString name_ __TA_GUARDED(info_mutex_);
  timespec atime_ __TA_GUARDED(info_mutex_) = {0, 0};
  timespec mtime_ __TA_GUARDED(info_mutex_) = {0, 0};
  timespec ctime_ __TA_GUARDED(info_mutex_) = {0, 0};

  fs::SharedMutex extent_cache_mutex_;                         // rwlock for consistency
  ExtentInfo extent_cache_ __TA_GUARDED(extent_cache_mutex_);  // in-memory extent cache entry

  const ino_t ino_ = 0;
  F2fs *const fs_ = nullptr;
  umode_t mode_ = 0;
  fs::WatcherContainer watcher_{};
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_VNODE_H_

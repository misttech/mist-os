// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/blob.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/sync/completion.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <algorithm>
#include <iterator>
#include <memory>
#include <utility>

#include "src/storage/blobfs/blob_layout.h"
#include "src/storage/blobfs/blob_verifier.h"
#include "src/storage/blobfs/blob_writer.h"
#include "src/storage/blobfs/blobfs.h"
#include "src/storage/blobfs/common.h"
#include "src/storage/blobfs/format.h"
#include "src/storage/blobfs/format_assertions.h"
#include "src/storage/blobfs/transaction.h"

namespace blobfs {

zx::result<> VerifyNullBlob(Blobfs& blobfs, const digest::Digest& digest) {
  zx::result verifier = BlobVerifier::CreateWithoutTree(digest, blobfs.GetMetrics(), 0,
                                                        &blobfs.blob_corruption_notifier());
  if (verifier.is_error()) {
    return verifier.take_error();
  }
  if (zx_status_t status = verifier->Verify(nullptr, 0, 0); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

uint64_t Blob::FileSize() const {
  std::lock_guard lock(mutex_);
  if (state_ == BlobState::kReadable)
    return blob_size_;
  return 0;
}

Blob::Blob(Blobfs& blobfs, const digest::Digest& digest, bool is_delivery_blob)
    : CacheNode(*blobfs.vfs(), digest), blobfs_(blobfs) {
  writer_ = std::make_unique<Blob::Writer>(*this, is_delivery_blob);
}

Blob::Blob(Blobfs& blobfs, uint32_t node_index, const Inode& inode)
    : CacheNode(*blobfs.vfs(), digest::Digest(inode.merkle_root_hash)),
      blobfs_(blobfs),
      state_(BlobState::kReadable),
      syncing_state_(SyncingState::kDone),
      map_index_(node_index),
      blob_size_(inode.blob_size),
      block_count_(inode.block_count) {}

bool Blob::IsDataLoaded() const {
  // Data is served out of the paged_vmo().
  return paged_vmo().is_valid();
}

zx_status_t Blob::MarkReadable(const WrittenBlob& written_blob) {
  if (readable_event_.is_valid()) {
    if (zx_status_t status = readable_event_.signal(0u, ZX_USER_SIGNAL_0); status != ZX_OK) {
      return OnWriteError(zx::error(status));
    }
  }
  map_index_ = written_blob.map_index;
  blob_size_ = written_blob.layout->FileSize();
  block_count_ = written_blob.layout->TotalBlockCount();
  state_ = BlobState::kReadable;
  syncing_state_ = SyncingState::kSyncing;
  writer_.reset();
  return ZX_OK;
}

zx_status_t Blob::GetReadableEvent(zx::event* out) {
  TRACE_DURATION("blobfs", "Blobfs::GetReadableEvent");
  zx_status_t status;
  // This is the first 'wait until read event' request received.
  if (!readable_event_.is_valid()) {
    status = zx::event::create(0, &readable_event_);
    if (status != ZX_OK) {
      return status;
    }
    if (state_ == BlobState::kReadable) {
      readable_event_.signal(0u, ZX_USER_SIGNAL_0);
    }
  }
  zx::event out_event;
  status = readable_event_.duplicate(ZX_RIGHTS_BASIC, &out_event);
  if (status != ZX_OK) {
    return status;
  }

  *out = std::move(out_event);
  return ZX_OK;
}

zx_status_t Blob::CloneDataVmo(zx_rights_t rights, zx::vmo* out_vmo) {
  TRACE_DURATION("blobfs", "Blobfs::CloneVmo", "rights", rights);

  if (state_ != BlobState::kReadable) {
    return ZX_ERR_BAD_STATE;
  }
  if (blob_size_ == 0) {
    return ZX_ERR_BAD_STATE;
  }

  if (zx_status_t status = LoadVmosFromDisk(); status != ZX_OK) {
    return status;
  }

  zx::vmo clone;
  if (zx_status_t status =
          paged_vmo().create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, 0, blob_size_, &clone);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to create child VMO";
    return status;
  }
  DidClonePagedVmo();

  // Only add exec right to VMO if explictly requested.  (Saves a syscall if we're just going to
  // drop the right back again in replace() call below.)
  if (rights & ZX_RIGHT_EXECUTE) {
    // Check if the VMEX resource held by Blobfs is valid and fail if it isn't. We do this to make
    // sure that we aren't implicitly relying on the ZX_POL_AMBIENT_MARK_VMO_EXEC job policy.
    const zx::resource& vmex = blobfs_.vmex_resource();
    if (!vmex.is_valid()) {
      FX_LOGS(ERROR) << "No VMEX resource available, executable blobs unsupported";
      return ZX_ERR_NOT_SUPPORTED;
    }
    if (zx_status_t status = clone.replace_as_executable(vmex, &clone); status != ZX_OK) {
      return status;
    }
  }

  // Narrow rights to those requested.
  if (zx_status_t status = clone.replace(rights, &clone); status != ZX_OK) {
    return status;
  }
  *out_vmo = std::move(clone);

  return ZX_OK;
}

zx_status_t Blob::ReadInternal(void* data, size_t len, size_t off, size_t* actual) {
  TRACE_DURATION("blobfs", "Blobfs::ReadInternal", "len", len, "off", off);

  // The common case is that the blob is already loaded. To allow multiple readers, it's important
  // to avoid taking an exclusive lock unless necessary.
  fs::SharedLock lock(mutex_);

  // Only expect this to be called when the blob is open. The fidl API guarantees this but tests
  // can easily forget to open the blob before trying to read.
  ZX_DEBUG_ASSERT(open_count() > 0);

  if (state_ != BlobState::kReadable)
    return ZX_ERR_BAD_STATE;

  if (!IsDataLoaded()) {
    // Release the shared lock and load the data from within an exclusive lock. LoadVmosFromDisk()
    // can be called multiple times so the race condition caused by this unlocking will be benign.
    lock.unlock();
    {
      // Load the VMO data from within the lock.
      std::lock_guard exclusive_lock(mutex_);
      if (zx_status_t status = LoadVmosFromDisk(); status != ZX_OK)
        return status;
    }
    lock.lock();

    // The readable state should never change (from the value we checked at the top of this
    // function) by attempting to load from disk, that only happens when we try to write.
    ZX_DEBUG_ASSERT(state_ == BlobState::kReadable);
  }

  if (blob_size_ == 0) {
    *actual = 0;
    return ZX_OK;
  }
  if (off >= blob_size_) {
    *actual = 0;
    return ZX_OK;
  }
  if (len > (blob_size_ - off)) {
    len = blob_size_ - off;
  }
  ZX_DEBUG_ASSERT(IsDataLoaded());

  // Send reads through the pager. This will potentially page-in the data by reentering us from the
  // kernel on the pager thread.
  ZX_DEBUG_ASSERT(paged_vmo().is_valid());
  if (zx_status_t status = paged_vmo().read(data, off, len); status != ZX_OK)
    return status;
  *actual = len;
  return ZX_OK;
}

zx_status_t Blob::LoadPagedVmosFromDisk() {
  ZX_ASSERT_MSG(!IsDataLoaded(), "Data VMO is not loaded.");

  // If there is an overridden cache policy for pager-backed blobs, apply it now. Otherwise the
  // system-wide default will be used.
  std::optional<CachePolicy> cache_policy = blobfs_.pager_backed_cache_policy();
  if (cache_policy) {
    set_overridden_cache_policy(*cache_policy);
  }

  zx::result<LoaderInfo> load_info_or =
      blobfs_.loader().LoadBlob(map_index_, &blobfs_.blob_corruption_notifier());
  if (load_info_or.is_error())
    return load_info_or.error_value();

  // Make the vmo.
  if (auto status = EnsureCreatePagedVmo(load_info_or->layout->FileBlockAlignedSize());
      status.is_error())
    return status.error_value();

  // Commit the other load information.
  loader_info_ = std::move(*load_info_or);

  return ZX_OK;
}

zx_status_t Blob::LoadVmosFromDisk() {
  // We expect the file to be open in FIDL for this to be called. Whether the paged vmo is
  // registered with the pager is dependent on the HasReferences() flag so this should not get
  // out-of-sync.
  ZX_DEBUG_ASSERT(HasReferences());

  if (IsDataLoaded())
    return ZX_OK;

  if (blob_size_ == 0) {
    // Null blobs don't need any loading, just verification that they're correct.
    return VerifyNullBlob(blobfs_, digest()).status_value();
  }

  zx_status_t status = LoadPagedVmosFromDisk();
  if (status == ZX_OK)
    SetPagedVmoName(true);

  syncing_state_ = SyncingState::kDone;
  return status;
}

zx_status_t Blob::QueueUnlink() {
  std::lock_guard lock(mutex_);

  deletable_ = true;
  // Attempt to purge in case the blob has been unlinked with no open fds
  return TryPurge();
}

zx_status_t Blob::Verify() {
  {
    std::lock_guard lock(mutex_);
    if (auto status = LoadVmosFromDisk(); status != ZX_OK)
      return status;
  }

  // For non-pager-backed blobs, commit the entire blob in memory. This will cause all of the pages
  // to be verified as they are read in (or for the null bob we just verify immediately). If the
  // commit operation fails due to a verification failure, we do propagate the error back via the
  // return status.
  //
  // This is a read-only operation on the blob so can be done with the shared lock. Since it will
  // reenter the Blob object on the pager thread to satisfy this request, it actually MUST be done
  // with only the shared lock or the reentrance on the pager thread will deadlock us.
  {
    fs::SharedLock lock(mutex_);

    // There is a race condition if somehow this blob was unloaded in between the above exclusive
    // lock and the shared lock in this block. Currently this is not possible because there is only
    // one thread processing fidl messages and paging events on the pager threads can't unload the
    // blob.
    //
    // But in the future certain changes might make this theoretically possible (though very
    // difficult to imagine in practice). If this were to happen, we would prefer to err on the side
    // of reporting a blob valid rather than mistakenly reporting errors that might cause a valid
    // blob to be deleted.
    if (state_ != BlobState::kReadable)
      return ZX_OK;

    if (blob_size_ == 0) {
      // It's the null blob, so just verify.
      return VerifyNullBlob(blobfs_, digest()).status_value();
    }
    return paged_vmo().op_range(ZX_VMO_OP_COMMIT, 0, blob_size_, nullptr, 0);
  }
}

void Blob::OnNoPagedVmoClones() {
  // Override the default behavior of PagedVnode to avoid clearing the paged_vmo. We keep this
  // alive for caching purposes as long as this object is alive, and this object's lifetime is
  // managed by the BlobCache.
  if (!HasReferences()) {
    // Mark the name to help identify the VMO is unused.
    SetPagedVmoName(false);
    // Hint that the VMO's pages are no longer needed, and can be evicted under memory pressure. If
    // a page is accessed again, it will lose the hint.
    zx_status_t status = paged_vmo().op_range(ZX_VMO_OP_DONT_NEED, 0, blob_size_, nullptr, 0);
    if (status != ZX_OK) {
      FX_LOGS(WARNING) << "Hinting DONT_NEED on blob " << digest()
                       << " failed: " << zx_status_get_string(status);
    }

    // This might have been the last reference to a deleted blob, so try purging it.
    if (zx_status_t status = TryPurge(); status != ZX_OK) {
      FX_LOGS(WARNING) << "Purging blob " << digest()
                       << " failed: " << zx_status_get_string(status);
    }
  }
}

BlobCache& Blob::GetCache() { return blobfs_.GetCache(); }

bool Blob::ShouldCache() const {
  std::lock_guard lock(mutex_);
  return state_ == BlobState::kReadable;
}

void Blob::ActivateLowMemory() {
  // The reference returned by FreePagedVmo() needs to be released outside of the lock since it
  // could be keeping this class in scope.
  fbl::RefPtr<fs::Vnode> pager_reference;
  {
    std::lock_guard lock(mutex_);

    // We shouldn't be putting the blob into a low-memory state while it is still mapped.
    //
    // It is common for tests to trigger this assert during Blobfs tear-down. This will happen when
    // the "no clones" message was not delivered before destruction. This can happen if the test
    // code kept a vmo reference, but can also happen when there are no clones because the delivery
    // of this message depends on running the message loop which is easy to skip in a test.
    //
    // Often, the solution is to call RunUntilIdle() on the loop after the test code has cleaned up
    // its mappings but before deleting Blobfs. This will allow the pending notifications to be
    // delivered.
    ZX_ASSERT_MSG(!has_clones(), "Cannot put blob in low memory state as its mapped via clones.");

    pager_reference = FreePagedVmo();

    loader_info_ = LoaderInfo();  // Release the verifiers and associated Merkle data.
  }
  // When the pager_reference goes out of scope here, it could delete |this|.
}

Blob::~Blob() { ActivateLowMemory(); }

fs::VnodeProtocolSet Blob::GetProtocols() const { return fs::VnodeProtocol::kFile; }

bool Blob::ValidateRights(fs::Rights rights) const {
  // To acquire write access to a blob, it must be empty.
  //
  // TODO(fxbug.dev/67659) If we run FIDL on multiple threads (we currently don't) there is a race
  // condition here where another thread could start writing at the same time. Decide whether we
  // support FIDL from multiple threads and if so, whether this condition is important.
  std::lock_guard lock(mutex_);
  return !rights.write || state_ == BlobState::kEmpty;
}

zx_status_t Blob::GetNodeInfoForProtocol([[maybe_unused]] fs::VnodeProtocol protocol,
                                         [[maybe_unused]] fs::Rights rights,
                                         fs::VnodeRepresentation* info) {
  std::lock_guard lock(mutex_);

  zx::event observer;
  if (zx_status_t status = GetReadableEvent(&observer); status != ZX_OK) {
    return status;
  }
  *info = fs::VnodeRepresentation::File{.observer = std::move(observer)};
  return ZX_OK;
}

zx_status_t Blob::Read(void* data, size_t len, size_t off, size_t* out_actual) {
  TRACE_DURATION("blobfs", "Blob::Read", "len", len, "off", off);
  return blobfs_.node_operations().read.Track(
      [&] { return ReadInternal(data, len, off, out_actual); });
}

zx_status_t Blob::Write(const void* data, size_t len, size_t offset, size_t* out_actual) {
  TRACE_DURATION("blobfs", "Blob::Write", "len", len, "off", offset);
  return blobfs_.node_operations().write.Track([&]() -> zx_status_t {
    std::lock_guard lock(mutex_);
    *out_actual = 0;
    if (state_ == BlobState::kError) {
      ZX_DEBUG_ASSERT(writer_);
      return writer_->status().error_value();
    }
    if (len == 0) {
      return ZX_OK;
    }
    if (!writer_ || state_ != BlobState::kDataWrite) {
      return ZX_ERR_BAD_STATE;
    }
    if (offset != writer_->total_written()) {
      FX_LOGS(ERROR) << "only append is currently supported (requested_offset: " << offset
                     << ", expected: " << writer_->total_written() << ")";
      return ZX_ERR_NOT_SUPPORTED;
    }

    // Perform the actual write.
    ZX_DEBUG_ASSERT(state_ == BlobState::kDataWrite);
    zx::result written_blob = writer_->Write(*this, data, len, out_actual);
    if (written_blob.is_error()) {
      return OnWriteError(written_blob.take_error());
    }

    if ((*written_blob).has_value()) {
      return MarkReadable(*written_blob.value());
    }

    return ZX_OK;  // More data to write.
  });
}

zx_status_t Blob::Append(const void* data, size_t len, size_t* out_end, size_t* out_actual) {
  TRACE_DURATION("blobfs", "Blob::Append", "len", len);
  return blobfs_.node_operations().append.Track([&]() -> zx_status_t {
    std::lock_guard lock(mutex_);
    *out_actual = 0;
    if (state_ == BlobState::kError) {
      ZX_DEBUG_ASSERT(writer_);
      return writer_->status().error_value();
    }
    if (len == 0) {
      return ZX_OK;
    }
    if (!writer_ || state_ != BlobState::kDataWrite) {
      return ZX_ERR_BAD_STATE;
    }

    // Perform the actual write.
    zx::result written_blob = writer_->Write(*this, data, len, out_actual);
    if (written_blob.is_error()) {
      return OnWriteError(written_blob.take_error());
    }
    *out_end = writer_->total_written();

    if ((*written_blob).has_value()) {
      return MarkReadable(*written_blob.value());
    }

    return ZX_OK;  // More data to write.
  });
}

zx_status_t Blob::GetAttributes(fs::VnodeAttributes* a) {
  TRACE_DURATION("blobfs", "Blob::GetAttributes");
  return blobfs_.node_operations().get_attr.Track([&] {
    // FileSize() expects to be called outside the lock.
    auto content_size = FileSize();

    std::lock_guard lock(mutex_);

    *a = fs::VnodeAttributes();
    a->mode = V_TYPE_FILE | V_IRUSR | V_IXUSR;
    a->inode = map_index_;
    a->content_size = content_size;
    a->storage_size = block_count_ * GetBlockSize();
    a->link_count = 1;
    a->creation_time = 0;
    a->modification_time = 0;
    return ZX_OK;
  });
}

zx_status_t Blob::Truncate(size_t len) {
  TRACE_DURATION("blobfs", "Blob::Truncate", "len", len);
  return blobfs_.node_operations().truncate.Track([this, len]() -> zx_status_t {
    std::lock_guard lock(mutex_);
    if (state_ != BlobState::kEmpty || writer_ == nullptr) {
      return ZX_ERR_BAD_STATE;
    }

    // Special case: If this is the null blob, we skip the write phase.
    if (len == 0) {
      zx::result written_blob = writer_->WriteNullBlob(*this);
      if (written_blob.is_error()) {
        return OnWriteError(written_blob.take_error());
      }
      return MarkReadable(*written_blob);
    }

    // Prepare writer_ to accept `len` bytes of data in total.
    zx::result status = writer_->Prepare(*this, len);
    if (status.is_error()) {
      return OnWriteError(status.take_error());
    }

    // Indicate that the blob is in the writable state now.
    state_ = BlobState::kDataWrite;
    return ZX_OK;
  });
}

zx::result<std::string> Blob::GetDevicePath() const {
  return blobfs_.Device()->GetTopologicalPath();
}

zx_status_t Blob::GetVmo(fuchsia_io::wire::VmoFlags flags, zx::vmo* out_vmo) {
  static_assert(sizeof flags == sizeof(uint32_t),
                "Underlying type of |flags| has changed, update conversion below.");
  TRACE_DURATION("blobfs", "Blob::GetVmo", "flags", static_cast<uint32_t>(flags));

  std::lock_guard lock(mutex_);

  // Only expect this to be called when the blob is open. The fidl API guarantees this but tests
  // can easily forget to open the blob before getting the VMO.
  ZX_DEBUG_ASSERT(open_count() > 0);

  if (flags & fuchsia_io::wire::VmoFlags::kWrite) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (flags & fuchsia_io::wire::VmoFlags::kSharedBuffer) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // Let clients map and set the names of their VMOs.
  zx_rights_t rights = ZX_RIGHTS_BASIC | ZX_RIGHT_MAP | ZX_RIGHTS_PROPERTY;
  // We can ignore VmoFlags::PRIVATE_CLONE since private / shared access to the underlying VMO can
  // both be satisfied with a clone due to the immutability of blobfs blobs.
  rights |= (flags & fuchsia_io::wire::VmoFlags::kRead) ? ZX_RIGHT_READ : 0;
  rights |= (flags & fuchsia_io::wire::VmoFlags::kExecute) ? ZX_RIGHT_EXECUTE : 0;
  return CloneDataVmo(rights, out_vmo);
}

void Blob::Sync(SyncCallback on_complete) {
  // This function will issue its callbacks on either the current thread or the journal thread. The
  // vnode interface says this is OK.
  TRACE_DURATION("blobfs", "Blob::Sync");
  auto event = blobfs_.node_operations().sync.NewEvent();
  // Wraps `on_complete` to record the result into `event` as well.
  SyncCallback completion_callback = [on_complete = std::move(on_complete),
                                      event = std::move(event)](zx_status_t status) mutable {
    on_complete(status);
    event.SetStatus(status);
  };

  SyncingState state;
  {
    std::scoped_lock guard(mutex_);
    state = syncing_state_;
  }

  switch (state) {
    case SyncingState::kDataIncomplete: {
      // It doesn't make sense to sync a partial blob since it can't have its proper
      // content-addressed name without all the data.
      completion_callback(ZX_ERR_BAD_STATE);
      break;
    }
    case SyncingState::kSyncing: {
      // The blob data is complete. When this happens the Blob object will automatically write its
      // metadata, but it may not get flushed for some time. This call both encourages the sync to
      // happen "soon" and provides a way to get notified when it does.
      auto trace_id = TRACE_NONCE();
      TRACE_FLOW_BEGIN("blobfs", "Blob.sync", trace_id);
      blobfs_.Sync(std::move(completion_callback));
      break;
    }
    case SyncingState::kDone: {
      // All metadata has already been synced. Calling Sync() is a no-op.
      completion_callback(ZX_OK);
      break;
    }
  }
}

// This function will get called on an arbitrary pager worker thread.
void Blob::VmoRead(uint64_t offset, uint64_t length) {
  TRACE_DURATION("blobfs", "Blob::VmoRead", "offset", offset, "length", length);

  // It's important that this function use only a shared read lock. This is for performance (to
  // allow multiple page requests to be run in parallel) and to prevent deadlock with the non-paged
  // Read() path. The non-paged path is implemented by reading from the vmo which will recursively
  // call into this code and taking an exclusive lock would deadlock.
  fs::SharedLock lock(mutex_);

  if (!paged_vmo()) {
    // Races with calling FreePagedVmo() on another thread can result in stale read requests. Ignore
    // them if the VMO is gone.
    return;
  }

  ZX_DEBUG_ASSERT(IsDataLoaded());

  std::optional vfs_opt = vfs();
  ZX_ASSERT(vfs_opt.has_value());
  fs::PagedVfs& vfs = vfs_opt.value().get();

  if (is_corrupt_) {
    FX_LOGS(ERROR) << "Blobfs failing page request because blob was previously found corrupt.";
    if (auto error_result = vfs.ReportPagerError(paged_vmo(), offset, length, ZX_ERR_BAD_STATE);
        error_result.is_error()) {
      FX_LOGS(ERROR) << "Failed to report pager error to kernel: " << error_result.status_string();
    }
    return;
  }

  auto page_supplier = PageLoader::PageSupplier(
      [&vfs, &dest_vmo = paged_vmo()](uint64_t offset, uint64_t length, const zx::vmo& aux_vmo,
                                      uint64_t aux_offset) {
        return vfs.SupplyPages(dest_vmo, offset, length, aux_vmo, aux_offset);
      });
  PagerErrorStatus pager_error_status =
      blobfs_.page_loader().TransferPages(page_supplier, offset, length, loader_info_);
  if (pager_error_status != PagerErrorStatus::kOK) {
    FX_LOGS(ERROR) << "Pager failed to transfer pages to the blob, error: "
                   << zx_status_get_string(static_cast<zx_status_t>(pager_error_status));
    if (auto error_result = vfs.ReportPagerError(paged_vmo(), offset, length,
                                                 static_cast<zx_status_t>(pager_error_status));
        error_result.is_error()) {
      FX_LOGS(ERROR) << "Failed to report pager error to kernel: " << error_result.status_string();
    }

    // We've signaled a failure and unblocked outstanding page requests for this range. If the pager
    // error was a verification error, fail future requests as well - we should not service further
    // page requests on a corrupt blob.
    //
    // Note that we cannot simply detach the VMO from the pager here. There might be outstanding
    // page requests which have been queued but are yet to be serviced. These need to be handled
    // correctly - if the VMO is detached, there will be no way for us to communicate failure to
    // the kernel, since zx_pager_op_range() requires a valid pager VMO handle. Without being able
    // to make a call to zx_pager_op_range() to indicate a failed page request, the faulting thread
    // would hang indefinitely.
    if (pager_error_status == PagerErrorStatus::kErrDataIntegrity)
      is_corrupt_ = true;
  }
}

bool Blob::HasReferences() const { return open_count() > 0 || has_clones(); }

void Blob::CompleteSync() {
  // Called on the journal thread when the syncing is complete.
  {
    std::scoped_lock guard(mutex_);
    syncing_state_ = SyncingState::kDone;
  }
}

void Blob::WillTeardownFilesystem() {
  // Be careful to release the pager reference outside the lock.
  fbl::RefPtr<fs::Vnode> pager_reference;
  {
    std::lock_guard lock(mutex_);
    pager_reference = FreePagedVmo();
  }
  // When pager_reference goes out of scope here, it could cause |this| to be deleted.
}

zx_status_t Blob::OpenNode([[maybe_unused]] ValidatedOptions options,
                           fbl::RefPtr<Vnode>* out_redirect) {
  std::lock_guard lock(mutex_);
  if (IsDataLoaded() && open_count() == 1) {
    // Just went from an unopened node that already had data to an opened node (the open_count()
    // reflects the new state).
    //
    // This normally means that the node was closed but cached, and we're not re-opening it. This
    // means we have to mark things as being open and register for the corresponding notifications.
    //
    // It's also possible to get in this state if there was a memory mapping for a file that
    // was otherwise closed. In that case we don't need to do anything but the operations here
    // can be performed multiple times with no bad effects. Avoiding these calls in the "mapped but
    // opened" state would mean checking for no mappings which bundles this code more tightly to
    // the HasReferences() implementation that is better avoided.
    SetPagedVmoName(true);
  }
  return ZX_OK;
}

zx_status_t Blob::CloseNode() {
  TRACE_DURATION("blobfs", "Blob::CloseNode");
  return blobfs_.node_operations().close.Track([&] {
    std::lock_guard lock(mutex_);

    if (paged_vmo() && !HasReferences()) {
      // Mark the name to help identify the VMO is unused.
      SetPagedVmoName(false);
      // Hint that the VMO's pages are no longer needed, and can be evicted under memory pressure.
      // If a page is accessed again, it will lose the hint.
      zx_status_t status = paged_vmo().op_range(ZX_VMO_OP_DONT_NEED, 0, blob_size_, nullptr, 0);
      if (status != ZX_OK) {
        FX_LOGS(WARNING) << "Hinting DONT_NEED on blob " << digest()
                         << " failed: " << zx_status_get_string(status);
      }
    }

    // Attempt purge in case blob was unlinked prior to close.
    return TryPurge();
  });
}

zx_status_t Blob::TryPurge() {
  if (Purgeable()) {
    return Purge();
  }
  return ZX_OK;
}

zx_status_t Blob::Purge() {
  ZX_DEBUG_ASSERT(Purgeable());

  if (state_ == BlobState::kReadable) {
    // A readable blob should only be purged if it has been unlinked.
    ZX_ASSERT_MSG(deletable_, "Should not purge blob which is not unlinked.");

    BlobTransaction transaction;
    if (zx_status_t status = blobfs_.FreeInode(map_index_, transaction); status != ZX_OK)
      return status;
    transaction.Commit(*blobfs_.GetJournal());
    blobfs_.GetAllocator()->Decommit();
  }

  // If the blob is in the error state, it should have already been evicted from
  // the cache (see Blob::OnWriteError).
  if (state_ != BlobState::kError) {
    if (zx_status_t status = GetCache().Evict(fbl::RefPtr(this)); status != ZX_OK)
      return status;
  }

  state_ = BlobState::kPurged;
  return ZX_OK;
}

uint64_t Blob::GetBlockSize() const { return blobfs_.Info().block_size; }

void Blob::SetPagedVmoName(bool active) {
  VmoNameBuffer name =
      active ? FormatBlobDataVmoName(digest()) : FormatInactiveBlobDataVmoName(digest());
  // Ignore failures, the name is for informational purposes only.
  paged_vmo().set_property(ZX_PROP_NAME, name.data(), name.size());
}

zx_status_t Blob::OnWriteError(zx::error_result error) {
  ZX_DEBUG_ASSERT(writer_);
  writer_->set_status(error);
  state_ = BlobState::kError;
  // Evict this blob from the cache now that we're placing it in an error state.
  if (zx_status_t status = GetCache().Evict(fbl::RefPtr(this)); status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to evict blob from cache: " << zx_status_get_string(status);
  }
  // Return the now latched write error.
  return writer_->status().error_value();
}

}  // namespace blobfs

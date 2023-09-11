// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_STORAGE_BUFFER_H_
#define SRC_STORAGE_F2FS_STORAGE_BUFFER_H_

namespace f2fs {

class VmoBufferKey : public fbl::DoublyLinkedListable<std::unique_ptr<VmoBufferKey>> {
 public:
  VmoBufferKey(const uint64_t offset) : vmo_offset_(offset) {}
  VmoBufferKey() = delete;
  VmoBufferKey(const VmoBufferKey &) = delete;
  VmoBufferKey &operator=(const VmoBufferKey &) = delete;
  VmoBufferKey(const VmoBufferKey &&) = delete;
  VmoBufferKey &operator=(const VmoBufferKey &&) = delete;
  uint64_t GetKey() const { return vmo_offset_; }

 private:
  const uint64_t vmo_offset_;
};

class StorageOperations;
using VmoKeyList = fbl::SizedDoublyLinkedList<std::unique_ptr<VmoBufferKey>>;

// StorageBuffer implements an allocator for pre-allocated vmo buffers attached to a VmoidRegistry
// object. When there are available buffers in the free list, allocation operations are O(1). If the
// free list is empty, a caller waits for buffers. Free operations are O(1) as well.
class StorageBuffer {
 public:
  // StorageBuffer reserves vmo buffers in |allocation_unit|. Therefore, |allocation_unit| should be
  // bigger than the number of pages requested in |ReserveWriteOperation| or |ReserveReadOperations|
  // to get the maximum performance. It should be also smaller than |blocks|, because |blocks| is
  // the total size of vmo buffers.
  StorageBuffer(BcacheMapper *bc, size_t blocks, uint32_t block_size, std::string_view label,
                uint32_t allocation_unit = 1);
  StorageBuffer() = delete;
  StorageBuffer(const StorageBuffer &) = delete;
  StorageBuffer &operator=(const StorageBuffer &) = delete;
  StorageBuffer(const StorageBuffer &&) = delete;
  StorageBuffer &operator=(const StorageBuffer &&) = delete;
  ~StorageBuffer();

  // It tries to reserve |buffer_| for |page| for writeback. If successful,
  // it copies the contents of |page| to the reserved buffer.
  zx::result<size_t> ReserveWriteOperation(Page &page) __TA_EXCLUDES(mutex_);
  // It returns StorageOperations from |reserved_keys_| and |builder| for writeback.
  StorageOperations TakeWriteOperations() __TA_EXCLUDES(mutex_);

  // It tries to reserve |buffer_| for read I/Os. If successful, it returns StorageOpeartions that
  // convey BufferedOperations to read blocks for |addrs|.
  zx::result<StorageOperations> MakeReadOperations(const std::vector<block_t> &addrs)
      __TA_EXCLUDES(mutex_);

  void ReleaseBuffers(const StorageOperations &operation) __TA_EXCLUDES(mutex_);
  const void *Data(const size_t offset) const {
    ZX_ASSERT(offset < buffer_.capacity());
    return buffer_.Data(offset);
  }

 private:
  void Init() __TA_EXCLUDES(mutex_);
  const uint64_t max_blocks_;
  const uint32_t allocation_unit_;
  storage::VmoBuffer buffer_;
  fs::BufferedOperationsBuilder builder_ __TA_GUARDED(mutex_);
  VmoKeyList free_keys_ __TA_GUARDED(mutex_);
  VmoKeyList reserved_keys_ __TA_GUARDED(mutex_);
  std::condition_variable_any cvar_;
  fs::SharedMutex mutex_;
};

using OperationCallback = fit::function<void(const StorageOperations &, zx_status_t status)>;
// A utility class, holding a collection of write requests with data buffers of
// StorageBuffer, ready to be transmitted to persistent storage.
class StorageOperations {
 public:
  StorageOperations() = delete;
  StorageOperations(StorageBuffer &buffer, fs::BufferedOperationsBuilder &operations,
                    VmoKeyList &keys)
      : buffer_(buffer), operations_(operations.TakeOperations()), keys_(std::move(keys)) {}
  StorageOperations(const StorageOperations &operations) = delete;
  StorageOperations &operator=(const StorageOperations &) = delete;
  StorageOperations(const StorageOperations &&op) = delete;
  StorageOperations &operator=(const StorageOperations &&) = delete;
  StorageOperations(StorageOperations &&op) = default;
  StorageOperations &operator=(StorageOperations &&) = delete;
  ~StorageOperations() { ZX_DEBUG_ASSERT(keys_.is_empty()); }

  std::vector<storage::BufferedOperation> TakeOperations() { return std::move(operations_); }

  // When StorageOperations complete, Reader or Writer should call it to release storage
  // buffers and handle the IO completion in |callback|.
  zx_status_t Completion(zx_status_t io_status, OperationCallback callback) {
    callback(*this, io_status);
    buffer_.ReleaseBuffers(*this);
    return io_status;
  }
  bool IsEmpty() const { return keys_.is_empty(); }
  VmoKeyList TakeVmoKeys() const { return std::move(keys_); }
  VmoKeyList &VmoKeys() const { return keys_; }

 private:
  StorageBuffer &buffer_;
  std::vector<storage::BufferedOperation> operations_;
  mutable VmoKeyList keys_;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_STORAGE_BUFFER_H_

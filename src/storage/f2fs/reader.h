// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_READER_H_
#define SRC_STORAGE_F2FS_READER_H_

namespace f2fs {

class Reader {
 public:
  Reader(BcacheMapper *bc, size_t capacity);
  Reader() = delete;
  Reader(const Reader &) = delete;
  Reader &operator=(const Reader &) = delete;
  Reader(const Reader &&) = delete;
  Reader &operator=(const Reader &&) = delete;

  // It makes read operations from |buffer_| and passes them to RunReqeusts()
  // synchronously.
  zx::result<> ReadBlocks(std::vector<LockedPage> &pages, std::vector<block_t> &addrs);
  zx::result<> ReadBlocks(zx::vmo &vmo, std::vector<block_t> &addrs);

 private:
  zx_status_t RunIO(StorageOperations &operation, OperationCallback callback);

  BcacheMapper *bc_ = nullptr;
  std::unique_ptr<StorageBuffer> buffer_;
  static constexpr uint32_t kDefaultAllocationUnit_ = kDefaultReadaheadSize;
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_READER_H_

// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_VFS_CPP_JOURNAL_SUPERBLOCK_H_
#define SRC_STORAGE_LIB_VFS_CPP_JOURNAL_SUPERBLOCK_H_

#include <zircon/types.h>

#include <cstdint>
#include <memory>

#include <storage/buffer/block_buffer.h>

#include "src/storage/lib/vfs/cpp/journal/format.h"

namespace fs {

// Contains and manages state representing the on-device journal info block.
class JournalSuperblock {
 public:
  JournalSuperblock();
  explicit JournalSuperblock(std::unique_ptr<storage::BlockBuffer> buffer);

  // Confirms that the magic and checksums within the info block are correct.
  //
  // Returns |ZX_ERR_IO| if these fields do not match the expected value.
  zx_status_t Validate() const;

  // Updates all client-visible fields of the info block. Additionally updates the checksum and
  // sequence_number in-memory.
  void Update(uint64_t start, uint64_t sequence_number);

  // Returns the start of the first journal entry.
  uint64_t start() const { return Info()->start_block; }
  uint64_t sequence_number() const { return Info()->timestamp; }
  const storage::BlockBuffer& buffer() const { return *buffer_; }

 private:
  uint32_t new_checksum() const;

  uint32_t old_checksum() const { return Info()->checksum; }

  const JournalInfo* Info() const { return reinterpret_cast<const JournalInfo*>(buffer_->Data(0)); }

  JournalInfo* Info() { return reinterpret_cast<JournalInfo*>(buffer_->Data(0)); }

  std::unique_ptr<storage::BlockBuffer> buffer_;
};

}  // namespace fs

#endif  // SRC_STORAGE_LIB_VFS_CPP_JOURNAL_SUPERBLOCK_H_

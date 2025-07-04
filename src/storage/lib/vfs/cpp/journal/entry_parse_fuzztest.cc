// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stddef.h>
#include <stdint.h>
#include <zircon/errors.h>

#include <vector>

#include <storage/buffer/vmo_buffer.h>
#include <storage/operation/operation.h>

#include "src/storage/lib/vfs/cpp/journal/fuzzer_utils.h"
#include "src/storage/lib/vfs/cpp/journal/replay.h"
#include "src/storage/lib/vfs/cpp/journal/superblock.h"

namespace fs {
namespace {

extern "C" int LLVMFuzzerTestOneInput(uint8_t *data, size_t size) {
  FuzzerUtils fuzz_utils(data, size);
  JournalSuperblock info;
  storage::VmoBuffer journal_buffer;
  if (fuzz_utils.FuzzSuperblock(&info) != ZX_OK ||
      fuzz_utils.FuzzJournal(&journal_buffer) != ZX_OK) {
    return 0;
  }
  // Fuzz `ParseJournalEntries`.
  std::vector<storage::BufferedOperation> operations;
  uint64_t sequence_number;
  uint64_t start;
  ParseJournalEntries(&info, &journal_buffer, &operations, &sequence_number, &start);
  return 0;
}

}  // namespace
}  // namespace fs

// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <string.h>
#include <zircon/assert.h>
#include <zircon/types.h>

#include <fuzzer/FuzzedDataProvider.h>

#include "src/storage/lib/vfs/cpp/vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vmo_file.h"

static const size_t kMaxVmoSz = 1024 * 1024 * 40;
static const size_t kMaxWriteSz = 4096;
static uint8_t read_buffer[kMaxWriteSz];

extern "C" int LLVMFuzzerTestOneInput(const uint8_t* data, size_t size) {
  FuzzedDataProvider fuzzed_data(data, size);
  uint64_t vmo_size = fuzzed_data.ConsumeIntegralInRange<uint64_t>(1, kMaxVmoSz);
  zx::vmo vmo;
  auto vmo_flags = fuzzed_data.PickValueInArray<uint32_t>({0, ZX_VMO_RESIZABLE});
  zx_status_t res = zx::vmo::create(vmo_size, vmo_flags, &vmo);
  if (res != ZX_OK) {
    return 0;
  }

  size_t length = fuzzed_data.ConsumeIntegral<size_t>();
  fs::VmoFile::DefaultSharingMode vmo_sharing = fuzzed_data.PickValueInArray(
      {fs::VmoFile::DefaultSharingMode::kNone, fs::VmoFile::DefaultSharingMode::kDuplicate,
       fs::VmoFile::DefaultSharingMode::kCloneCow});

  auto vmo_file =
      fbl::MakeRefCounted<fs::VmoFile>(std::move(vmo), length, true /*writable*/, vmo_sharing);

  size_t offset = fuzzed_data.ConsumeIntegralInRange<size_t>(0, vmo_size);
  size_t bytes_written = 0;
  size_t bytes_read = 0;

  std::vector<uint8_t> to_write =
      fuzzed_data.ConsumeBytes<uint8_t>(fuzzed_data.ConsumeIntegralInRange<size_t>(0, kMaxWriteSz));

  res = vmo_file->Write(to_write.data(), to_write.size(), offset, &bytes_written);
  if (res == ZX_OK) {
    res = vmo_file->Read(read_buffer, bytes_written, offset, &bytes_read);
    assert(res == ZX_OK);
    assert(bytes_read == bytes_written);
    assert(memcmp(read_buffer, to_write.data(), bytes_read) == 0);
  }
  return 0;
}

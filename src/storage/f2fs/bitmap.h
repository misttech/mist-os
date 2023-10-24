// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_BITMAP_H_
#define SRC_STORAGE_F2FS_BITMAP_H_

#include <limits.h>
#include <stddef.h>
#include <zircon/types.h>

#include <bitmap/raw-bitmap.h>
#include <bitmap/storage.h>
#include <fbl/macros.h>
#include <fbl/ref_ptr.h>
#include <safemath/checked_math.h>

#include "src/storage/f2fs/f2fs_types.h"

namespace f2fs {

constexpr size_t kLastNodeMask = 0x7;

using RawBitmap = bitmap::RawBitmapGeneric<bitmap::VmoStorage>;
using RawBitmapHeap = bitmap::RawBitmapGeneric<bitmap::DefaultStorage>;

class Page;
class PageBitmap {
 public:
  PageBitmap() = delete;
  PageBitmap(const PageBitmap &bits) = delete;
  PageBitmap &operator=(const PageBitmap &bits) = delete;
  PageBitmap &operator=(PageBitmap &&bits) = delete;

  PageBitmap(fbl::RefPtr<Page> page, void *bitmap, size_t size);
  PageBitmap(void *bitmap, size_t size);
  PageBitmap(PageBitmap &&bits) = default;

  bool Set(size_t pos);
  bool Clear(size_t pos);
  bool Test(size_t pos) const;
  size_t GetSize() const;

  size_t FindNextZeroBit(size_t pos) const { return FindNextZeroBit(pos, size_); }
  size_t FindNextZeroBit(size_t pos, size_t max_pos) const;
  size_t FindNextBit(size_t pos) const { return FindNextBit(pos, size_); }
  size_t FindNextBit(size_t pos, size_t max_pos) const;

  // This method allows access to underlying storage but is dangerous:
  // It leaks the pointer to |bits_|. A caller may use it to import or
  // export bitmaps by using memcpy(). ~PageBitmap() should not be called
  // while the pointer returned from GetData() is alive.

  void *GetData() const { return bits_; }

 private:
  uint8_t &GetNode(size_t pos) { return bits_[pos >> kShiftForBitSize]; }
  const uint8_t &GetNode(size_t pos) const { return bits_[pos >> kShiftForBitSize]; }

  fbl::RefPtr<Page> page_;
  uint8_t *const bits_ = nullptr;
  const size_t size_ = 0;
};

template <typename T = uint8_t>
inline T GetMask(const T value, const size_t offset) {
  return static_cast<T>(value << offset);
}

inline size_t GetBitSize(const size_t size_in_byte) {
  return safemath::CheckLsh(size_in_byte, kShiftForBitSize).ValueOrDie();
}

inline size_t ToMsbFirst(const size_t offset) {
  size_t index_in_last_node = offset & kLastNodeMask;
  return offset + 7 - 2 * index_in_last_node;
}

size_t CountBits(const RawBitmap &bits, size_t offset, size_t len);

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_BITMAP_H_

// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_F2FS_LIB_H_
#define SRC_STORAGE_F2FS_F2FS_LIB_H_

#include <safemath/checked_math.h>

namespace f2fs {

// Checkpoint
inline bool VerAfter(uint64_t a, uint64_t b) { return a > b; }

// CRC
inline uint32_t F2fsCalCrc32(uint32_t crc, void *buff, uint32_t len) {
  unsigned char *p = static_cast<unsigned char *>(buff);
  while (len-- > 0) {
    crc ^= *p++;
    for (int i = 0; i < 8; ++i)
      crc = (crc >> 1) ^ ((crc & 1) ? kCrcPolyLe : 0);
  }
  return crc;
}

inline uint32_t F2fsCrc32(void *buff, uint32_t len) {
  return F2fsCalCrc32(kF2fsSuperMagic, static_cast<unsigned char *>(buff), len);
}

inline bool F2fsCrcValid(uint32_t blk_crc, void *buff, uint32_t buff_size) {
  return F2fsCrc32(buff, buff_size) == blk_crc;
}

inline bool IsDotOrDotDot(std::string_view name) { return (name == "." || name == ".."); }

template <typename T>
inline T CheckedDivRoundUp(const T n, const T d) {
  return safemath::CheckDiv<T>(fbl::round_up(n, d), d).ValueOrDie();
}

constexpr uint32_t kBlockSize = 4096;  // F2fs block size in byte
template <typename T = uint8_t>
class FsBlock {
 public:
  FsBlock() { memset(data_, 0, kBlockSize); }
  FsBlock(uint8_t (&block)[kBlockSize]) { std::memcpy(data_, block, kBlockSize); }
  FsBlock(const FsBlock &block) = delete;
  FsBlock &operator=(const FsBlock &block) = delete;
  FsBlock &operator=(const uint8_t (&block)[kBlockSize]) {
    std::memcpy(data_, block, kBlockSize);
    return *this;
  }
  template <typename U = void>
  U *get() {
    return reinterpret_cast<U *>(data_);
  }
  T *operator->() { return get<T>(); }
  T *operator&() { return get<T>(); }
  T &operator*() { return *get<T>(); }

 private:
  uint64_t data_[kBlockSize / sizeof(uint64_t)];
};

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_F2FS_LIB_H_

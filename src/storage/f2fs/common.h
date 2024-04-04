// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_F2FS_COMMON_H_
#define SRC_STORAGE_F2FS_COMMON_H_

#include <sys/types.h>
#include <zircon/listnode.h>
#include <zircon/types.h>

#include <fbl/no_destructor.h>
#include <safemath/checked_math.h>

#include "src/storage/lib/vfs/cpp/paged_vfs.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace f2fs {

class Page;
class VnodeF2fs;

using block_t = uint32_t;
using f2fs_hash_t = uint32_t;
using nid_t = uint32_t;
using ino_t = uint32_t;
using pgoff_t = uint64_t;
using umode_t = uint16_t;
using VnodeCallback = fit::function<zx_status_t(fbl::RefPtr<VnodeF2fs> &)>;
using PageCallback = fit::function<zx_status_t(fbl::RefPtr<Page>)>;
using PageTaggingCallback = fit::function<zx_status_t(fbl::RefPtr<Page>, bool is_last_page)>;
using SyncCallback = fs::Vnode::SyncCallback;

// A async_dispatcher_t* is needed for some functions on Fuchsia only. In order to avoid ifdefs on
// every call that is compiled for host Fuchsia and Host, we define this as a nullptr_t type when
// compiling on host where callers should pass null and it's ignored.
//
// Prefer async_dispatcher_t* for Fuchsia-specific functions since it makes the intent more clear.
using FuchsiaDispatcher = async_dispatcher_t *;

// A reference to the vfs is needed for some functions. The specific vfs type is different between
// Fuchsia and Host, so define a PlatformVfs which represents the one for our current platform to
// avoid ifdefs on every call.
//
// Prefer using the appropriate vfs when the function is only used for one or the other.
using PlatformVfs = fs::PagedVfs;
using PageList = fbl::SizedDoublyLinkedList<fbl::RefPtr<Page>>;

#if BYTE_ORDER == BIG_ENDIAN
inline uint16_t LeToCpu(uint16_t x) { return SWAP_16(x); }
inline uint32_t LeToCpu(uint32_t x) { return SWAP_32(x); }
inline uint64_t LeToCpu(uint64_t x) { return SWAP_64(x); }
inline uint16_t CpuToLe(uint16_t x) { return SWAP_16(x); }
inline uint32_t CpuToLe(uint32_t x) { return SWAP_32(x); }
inline uint64_t CpuToLe(uint64_t x) { return SWAP_64(x); }
#else
inline uint16_t LeToCpu(uint16_t x) { return x; }
inline uint32_t LeToCpu(uint32_t x) { return x; }
inline uint64_t LeToCpu(uint64_t x) { return x; }
inline uint16_t CpuToLe(uint16_t x) { return x; }
inline uint32_t CpuToLe(uint32_t x) { return x; }
inline uint64_t CpuToLe(uint64_t x) { return x; }
#endif

constexpr uint32_t kPageSize = PAGE_SIZE;
constexpr uint32_t kBitsPerByte = 8;
constexpr uint32_t kShiftForBitSize = 3;
constexpr uint32_t kF2fsSuperMagic = 0xF2F52010;
constexpr uint32_t kCrcPolyLe = 0xedb88320;
constexpr size_t kWriteTimeOut = 60;   // in seconds
constexpr uint32_t kBlockSize = 4096;  // F2fs block size in byte

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

template <typename T = uint8_t>
class BlockBuffer {
 public:
  BlockBuffer(BlockBuffer &&block) = delete;
  BlockBuffer &operator=(BlockBuffer &&block) = delete;

  BlockBuffer() {
    Allocate();
    std::memset(data_, 0, kBlockSize);
  }

  BlockBuffer(const BlockBuffer &block) {
    Allocate();
    std::memcpy(data_, block.get(), kBlockSize);
  }

  BlockBuffer &operator=(const BlockBuffer &block) {
    std::memcpy(data_, block.get(), kBlockSize);
    return *this;
  }

  ~BlockBuffer() {
    if (!data_)
      return;
    std::free(data_);
  }

  template <typename U = void>
  U *get() {
    return static_cast<U *>(data_);
  }
  template <typename U = void>
  const U *get() const {
    return static_cast<U *>(data_);
  }

  T *operator->() { return get<T>(); }
  const T *operator->() const { return get<T>(); }

  T *operator&() { return get<T>(); }
  const T *operator&() const { return get<T>(); }

  T &operator*() { return *get<T>(); }
  const T &operator*() const { return *get<T>(); }

 private:
  void Allocate() {
    if (data_)
      return;
    data_ = std::aligned_alloc(kBlockSize, kBlockSize);
  }
  void *data_ = nullptr;
};

inline std::shared_mutex &GetGlobalLock() {
  static fbl::NoDestructor<std::shared_mutex> global_lock;
  return *global_lock;
}

}  // namespace f2fs

#endif  // SRC_STORAGE_F2FS_COMMON_H_

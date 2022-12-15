// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_MMAP_LOADER_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_MMAP_LOADER_H_

#include <sys/mman.h>
#include <unistd.h>

#include <cassert>
#include <cerrno>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "diagnostics.h"
#include "memory.h"
#include "posix.h"

namespace elfldltl {

class MmapLoader {
 public:
  ~MmapLoader() {
    if (!image().empty()) {
      munmap(image().data(), image().size());
    }
  }

  [[gnu::const]] static size_t page_size() { return sysconf(_SC_PAGESIZE); }

  // This takes a LoadInfo object describing segments to be mapped in and an opened file descriptor
  // from which the file contents should be mapped. It returns true on success and false otherwise,
  // in which case a diagnostic will be emitted to diag. In either case mappings may be present
  // until destruction of the MmapLoader object. Use Commit() to keep loaded pages mapped.
  // Logically, Commit() isn't sensible after Load has failed.
  template <class Diagnostics, class LoadInfo>
  bool Load(Diagnostics& diag, const LoadInfo& load_info, int fd) {
    // Make a mapping large enough to fit all segments. This mapping will be placed wherever the OS
    // wants, achieving ASLR. We will later map the segments at their specified offsets into this
    // mapping. PROT_NONE is important so that any holes in the layout of the binary will trap if
    // touched.
    void* map = mmap(nullptr, load_info.vaddr_size(), PROT_NONE, MAP_ANON | MAP_PRIVATE, -1, 0);
    if (map == MAP_FAILED) [[unlikely]] {
      diag.SystemError("couldn't mmap address range of size ", load_info.vaddr_size(), ": ",
                       PosixError{errno});
      return false;
    }
    memory_.set_image({static_cast<std::byte*>(map), load_info.vaddr_size()});
    memory_.set_base(load_info.vaddr_start());

    constexpr auto prot = [](const auto& s) constexpr {
      return (s.readable() ? PROT_READ : 0) | (s.writable() ? PROT_WRITE : 0) |
             (s.executable() ? PROT_EXEC : 0);
    };

    // Load segments are divided into 3 regions
    // [file pages]*[intersecting page]?[anon pages]*
    //
    // "file pages" are present when filesz > 0, "anon pages" will exist when memsz > filesz.
    // The "intersecting page" will exist when both the above are true and the filesz is not
    // a multiple of pagesize. Note, for the intersecting page we choose to map this as an
    // anonymous page then read in any remaining file data into it. The alternative would be
    // to map filesz page rounded up into memory and then zero out the zero fill portion of
    // the intersecting page. This isn't preferable because we would immeditely cause a page
    // fault and spend time zero'ing a page when the OS may already have a zero'd page for us.
    //
    // To map a segment we have 3 steps to map in each of the 3 different regions.
    //
    // 1. Map "file pages".
    //    If memsz > filesz, we round this size down to the nearest page, and will map in the
    //    remaining filesz as part of the intersecting page later. Otherwise, we can map in the
    //    entire filesz. We'll call the amount to map `map_size`.
    //
    // 2. Map "anon pages"
    //    Here we need to map memsz - `map_size` bytes, which will include the full intersectng
    //    page.
    //
    // 3. Fill in "intersecting page"
    //    pread in filesz - `map_size` into this page allocated in step 2.
    auto mapper = [base = reinterpret_cast<std::byte*>(map), vaddr_start = load_info.vaddr_start(),
                   prot, fd, &diag](const auto& segment) {
      std::byte* addr = base + (segment.vaddr() - vaddr_start);
      size_t map_size = segment.filesz();
      size_t zero_size = 0;
      size_t copy_size = 0;
      if (segment.memsz() > segment.filesz()) {
        copy_size = map_size & (page_size() - 1);
        map_size &= -page_size();
        zero_size = segment.memsz() - map_size;
      }

      if (map_size > 0) {
        if (mmap(addr, map_size, prot(segment), MAP_FIXED | MAP_PRIVATE, fd, segment.offset()) ==
            MAP_FAILED) [[unlikely]] {
          diag.SystemError("couldn't mmap ", map_size, " bytes at offset ", segment.offset(), ": ",
                           PosixError{errno});
          return false;
        }
        addr += map_size;
      }
      if (zero_size > 0) {
        if (mmap(addr, zero_size, prot(segment), MAP_FIXED | MAP_PRIVATE | MAP_ANON, -1, 0) ==
            MAP_FAILED) [[unlikely]] {
          diag.SystemError("couldn't mmap ", zero_size, " anonymous bytes: ", PosixError{errno});
          return false;
        }
      }
      if (copy_size > 0) {
        if (pread(fd, addr, copy_size, segment.offset() + map_size) !=
            static_cast<ssize_t>(copy_size)) [[unlikely]] {
          diag.SystemError("couldn't pread ", copy_size, " bytes ",
                           FileOffset{segment.offset() + map_size}, PosixError{errno});
          return false;
        }
      }

      return true;
    };

    return load_info.VisitSegments(mapper);
  }

  // This returns the DirectMemory of the mapping created by Load(). It should not be used after
  // destruction or after Commit(). If Commit() has been called before destruction then the
  // address range will continue to be usable, in which case one should save the object's
  // image() before Commit().
  DirectMemory& memory() { return memory_; }

  // Commit is used to keep the mapping created by Load around even after the MmapLoader object is
  // destroyed. This method is inherently the last thing called on the object if it is used.
  // Use like `std::move(loader).Commit();`
  void Commit() && { memory_.set_image({}); }

 private:
  cpp20::span<std::byte> image() { return memory_.image(); }

  DirectMemory memory_;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_MMAP_LOADER_H_

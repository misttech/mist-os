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
  MmapLoader() = default;

  explicit MmapLoader(size_t page_size) : page_size_(page_size) {}

  ~MmapLoader() {
    if (!image().empty()) {
      munmap(image().data(), image().size());
    }
  }

  MmapLoader(MmapLoader&& other) noexcept : memory_(other.memory_.image(), other.memory_.base()) {
    other.memory().set_image({});
  }

  MmapLoader& operator=(MmapLoader&& other) noexcept {
    memory_.set_image(other.memory_.image());
    memory_.set_base(other.memory_.base());
    other.memory_.set_image({});
    return *this;
  }

  [[gnu::const]] size_t page_size() const { return page_size_; }

  // This takes a LoadInfo object describing segments to be mapped in and an opened fd
  // from which the file contents should be mapped. It returns true on success and false otherwise,
  // in which case a diagnostic will be emitted to diag.
  //
  // When Load() is called, one should assume that the address space of the caller has a new mapping
  // whether the call succeeded or failed. The mapping is tied to the lifetime of the MmapLoader
  // until Commit() is called. Without committing, the destructor of the MmapLoader will destroy the
  // mapping.
  //
  // Logically, Commit() isn't sensible after Load has failed.
  template <class Diagnostics, class LoadInfo>
  [[nodiscard]] bool Load(Diagnostics& diag, const LoadInfo& load_info, int fd) {
    // Make a mapping large enough to fit all segments. This mapping will be placed wherever the OS
    // wants, achieving ASLR. We will later map the segments at their specified offsets into this
    // mapping. PROT_NONE is important so that any holes in the layout of the binary will trap if
    // touched.
    void* map = mmap(nullptr, load_info.vaddr_size(), PROT_NONE, MAP_ANON | MAP_PRIVATE, -1, 0);
    if (map == MAP_FAILED) [[unlikely]] {
      return diag.SystemError("couldn't mmap address range of size ", load_info.vaddr_size(), ": ",
                              PosixError{errno});
    }
    memory_.set_image({static_cast<std::byte*>(map), load_info.vaddr_size()});
    memory_.set_base(load_info.vaddr_start());

    constexpr auto prot = [](const auto& s) constexpr {
      return (s.readable() ? PROT_READ : 0) | (s.writable() ? PROT_WRITE : 0) |
             (s.executable() ? PROT_EXEC : 0);
    };

    // Load segments are divided into 2 or 3 regions depending on segment.
    // [file pages]*[intersecting page]?[anon pages]*
    //
    // * "file pages" are present when filesz > 0
    // * "anon pages" are present when memsz > filesz.
    // * "intersecting page" exists when both file pages and anon pages exist,
    //   and file pages are not an exact multiple of pagesize. At most a **single**
    //   intersecting page exists.
    //
    // **Note:**: The MmapLoader performs only two mappings.
    //    * Mapping file pages up to the last full page of file data.
    //    * Mapping anonymous pages, including the intersecting page, to the end of the segment.
    //
    // After the second mapping, the MmapLoader then reads in the partial file data into the
    // intersecting page.
    //
    // The alternative would be to map filesz page rounded up into memory and then zero out the
    // zero fill portion of the intersecting page. This isn't preferable because we would
    // immediately cause a page fault and spend time zero'ing a page when the OS may already have
    // copied this page for us.
    auto mapper = [base = reinterpret_cast<std::byte*>(map), vaddr_start = load_info.vaddr_start(),
                   prot, fd, &diag, this](const auto& segment) {
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

  /// Given a region returned by LoadInfo::RelroBounds, make that region read-only.
  template <class Diagnostics, class Region>
  [[nodiscard]] bool ProtectRelro(Diagnostics& diag, Region region) {
    if (!region.empty()) {
      auto ptr = reinterpret_cast<void*>(region.start + load_bias());
      if (mprotect(ptr, region.size(), PROT_READ) != 0) [[unlikely]] {
        diag.SystemError("cannot protect PT_GNU_RELRO region: ", PosixError{errno});
        return false;
      }
    }
    return true;
  }

  // After Load(), this is the bias added to the given LoadInfo::vaddr_start()
  // to find the runtime load address.
  uintptr_t load_bias() const {
    return reinterpret_cast<uintptr_t>(image().data()) - memory_.base();
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
  cpp20::span<std::byte> image() const { return memory_.image(); }
  uintptr_t base() const { return memory_.base(); }

  DirectMemory memory_;
  size_t page_size_ = sysconf(_SC_PAGESIZE);
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_MMAP_LOADER_H_

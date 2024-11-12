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
  // This is returned by Commit(), which completes the use of an MmapLoader.
  // It represents the capability to apply RELRO protections to a loaded image.
  // Unlike the MmapLoader object itself, its lifetime is not tied to the image
  // mappings.  After Commit(), the image mapping won't be destroyed by the
  // MmapLoader's destructor.
  class Relro {
   public:
    Relro() = default;

    // Movable, not copyable: the object represents capability ownership.
    Relro(const Relro&) = delete;
    Relro(Relro&&) = default;

    Relro& operator=(const Relro&) = delete;
    Relro& operator=(Relro&&) = default;

    // This is the only method that can be called, and it must be last.
    // It makes the RELRO region passed to MmapLoader::Commit read-only.
    template <class Diagnostics>
    [[nodiscard]] bool Commit(Diagnostics& diag) && {
      if (start_) {
        if (mprotect(start_, size_, PROT_READ) != 0) [[unlikely]] {
          diag.SystemError("cannot protect PT_GNU_RELRO region: ", PosixError{errno});
          return false;
        }
      }
      return true;
    }

   private:
    friend MmapLoader;

    template <class Region>
    Relro(const Region& region, uintptr_t load_bias) {
      if (!region.empty()) {
        start_ = reinterpret_cast<void*>(region.start + load_bias);
        size_ = region.size();
      }
    }

    void* start_ = nullptr;
    size_t size_ = 0;
  };

  explicit MmapLoader() : page_size_(sysconf(_SC_PAGESIZE)) {}

  explicit MmapLoader(size_t page_size) : page_size_(page_size) {}

  MmapLoader(MmapLoader&& other) noexcept
      : memory_{std::exchange(other.memory_, {})}, page_size_(other.page_size_) {}

  MmapLoader& operator=(MmapLoader&& other) noexcept {
    memory_ = std::exchange(other.memory_, {});
    page_size_ = other.page_size_;
    return *this;
  }

  ~MmapLoader() {
    if (!image().empty()) {
      munmap(image().data(), image().size());
    }
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

  // Commit is used to keep the mapping created by Load around even after the
  // MmapLoader object is destroyed.  It takes a RELRO region as returned by
  // LoadInfo::RelroBounds, and yields a Relro object (see above).  This method
  // is inherently the last thing called on the object if it is used.  Use like
  // `auto relro = std::move(loader).Commit(relro_bounds);`.  After any
  // relocation modifications to mapped segment memory, call
  // `std::move(relro).Commit();`.
  template <class Region>
  [[nodiscard]] Relro Commit(const Region& relro_bounds) && {
    Relro relro{relro_bounds, load_bias()};
    memory_.set_image({});
    return relro;
  }

 private:
  std::span<std::byte> image() const { return memory_.image(); }
  uintptr_t base() const { return memory_.base(); }

  DirectMemory memory_;
  size_t page_size_;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_MMAP_LOADER_H_

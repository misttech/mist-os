// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_VMAR_LOADER_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_VMAR_LOADER_H_

#include <lib/stdcompat/bit.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>

#include "diagnostics.h"
#include "memory.h"
#include "zircon.h"

namespace elfldltl {

/// This object encapsulates the work needed to load an object into a VMAR.
/// The Load function initializes the object state, and no functions should be called
/// unless Load returns successfully.
class VmarLoader {
 public:
  // TODO(91206): Enable root VMAR being specified out of process.
  explicit VmarLoader(const zx::vmar& vmar = *zx::vmar::root_self()) : vmar_(vmar.borrow()) {}

  ~VmarLoader() {
    if (child_vmar_) {
      // Confirm that child_vmar_ and direct memory state haven't diverged.
      ZX_DEBUG_ASSERT(!image().empty());
      child_vmar_.destroy();
    }
  }

  [[gnu::const]] static size_t page_size() { return zx_system_get_page_size(); }

  // This takes a LoadInfo object describing segments to be mapped in and a VMO
  // from which the file contents should be mapped. It returns true on success and false otherwise,
  // in which case a diagnostic will be emitted to diag.
  //
  // When Load() is called, one should assume that the address space of the caller has a new mapping
  // whether the call succeeded or failed. The mapping is tied to the lifetime of the VmarLoader
  // until Commit() is called. Without committing, the destructor of the VmarLoader will destroy the
  // mapping.
  template <class Diagnostics, class LoadInfo>
  [[nodiscard]] bool Load(Diagnostics& diag, const LoadInfo& load_info, zx::unowned_vmo vmo) {
    auto base_name_array = VmarLoader::GetVmoName(vmo->borrow());
    std::string_view base_name = std::string_view(base_name_array.data());

    // Allocate the child VMAR from the root VMAR and instantiate the direct memory.
    zx_status_t status = AllocateVmar(load_info.vaddr_size(), load_info.vaddr_start());

    if (status != ZX_OK) [[unlikely]] {
      diag.SystemError("Failed to allocate address space", ZirconError{status});
      return false;
    }

    // DataWithZeroFill segments are divided into 2 or 3 regions, depending on the segment.
    // [file pages]*[intersecting page]?[anon pages]*
    //
    // * "file pages" are present when filesz > 0, represented by 'map_size' below.
    // * "anon pages" are present when memsz > filesz, represented by 'zero_size' below.
    // * "intersecting page" exists when both file pages and anon pages exist,
    //   and file pages are not an exact multiple of pagesize. At most a SINGLE
    //   intersecting page exists. It is represented by 'copy_size' below.
    //
    // IMPLEMENTATION NOTE: The VmarLoader performs only two mappings.
    //    * Mapping file pages up to the last full page of file data.
    //    * Mapping anonymous pages, including the intersecting page, to the end of the segment.
    //
    // TODO(91206): Support mapping objects into VMAR from out of process.
    //
    // After the second mapping, the VmarLoader then reads in the partial file data into the
    // intersecting page.
    //
    // The alternative would be to map filesz page rounded up into memory and then zero out the
    // zero fill portion of the intersecting page. This isn't preferable because we would
    // immediately cause a page fault and spend time zero'ing a page when the OS may already have
    // copied this page for us.
    auto mapper = [this, vaddr_start = load_info.vaddr_start(), vmo, &diag, base_name,
                   num_mapped_anonymous_regions = size_t{0},
                   num_mapped_file_regions = size_t{0}](const auto& segment) mutable {
      // Where in the VMAR the segment begins, accounting for load bias.
      size_t vmar_offset = segment.vaddr() - vaddr_start;

      size_t map_size = segment.filesz();
      size_t zero_size = 0;
      size_t copy_size = 0;
      if (segment.memsz() > segment.filesz()) {
        copy_size = map_size & (page_size() - 1);
        map_size &= -page_size();
        zero_size = segment.memsz() - map_size;
      }

      const auto options = GetMapOptions(segment);

      if (map_size > 0) {
        zx_status_t status =
            this->MapFileData(segment.offset(), segment.writable(), options, map_size,
                              vmo->borrow(), base_name, vmar_offset, num_mapped_file_regions);

        if (status != ZX_OK) [[unlikely]] {
          diag.SystemError("Failed to map segment file data", ZirconError{status});
          return false;
        }

        // vmar_offset reused to determine offset of the anonymous pages. if we had file data
        // to map, we shift the vmar_offset to the end of the file data pages.
        vmar_offset += map_size;
      }

      if (zero_size > 0) {
        zx_status_t status = this->MapAnonymousData(options, zero_size, base_name, vmar_offset,
                                                    num_mapped_anonymous_regions);

        if (status != ZX_OK) [[unlikely]] {
          diag.SystemError("Failed to map segment anonymous data", ZirconError{status});
          return false;
        }
      }

      if (copy_size > 0) {
        size_t intersecting_page_offset = segment.offset() + map_size;
        zx_status_t status = vmo->read(reinterpret_cast<void*>(image().data() + vmar_offset),
                                       intersecting_page_offset, copy_size);

        if (status != ZX_OK) [[unlikely]] {
          diag.SystemError("failed to read in the remaining file data to the first anonymous page",
                           ZirconError{status});
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
  // TODO(91206): This API will not make sense once we support out of process loading.
  DirectMemory& memory() { return memory_; }

  // Commit is used to keep the mapping created by Load around even after the VmarLoader object is
  // destroyed. This method is inherently the last thing called on the object if it is used.
  // Use like `std::move(loader).Commit();`
  zx::vmar Commit() && {
    memory_.set_image({});
    return std::exchange(child_vmar_, {});
  }

 private:
  static constexpr std::string_view kVmoNamePrefixData = "data";
  static constexpr std::string_view kVmoNamePrefixBss = "bss";

  using VmoName = std::array<char, ZX_MAX_NAME_LEN>;
  static VmoName GetVmoName(zx::unowned_vmo vmo);

  template <typename SegmentType>
  static zx_vm_option_t GetMapOptions(const SegmentType& s) {
    // Permissions on Fuchsia are trickier than Linux, so read these comments carefully if you
    // are encountering permission issues!

    // 1) Every segment is mapped to a specific location in the child VMAR. The permission
    //    to do so is given by the ZX_VM_CAN_MAP_SPECIFIC permission on the fresh allocation.
    // 2) ZX_VM_ALLOW_FAULTS is required by the kernel when mapping resizable or pager-backed
    //    VMOs, which we might be.
    constexpr zx_vm_option_t common_options = ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS;

    return (s.readable() ? ZX_VM_PERM_READ : 0) | (s.writable() ? ZX_VM_PERM_WRITE : 0) |
           // On some systems, execute-only pages are unsupported. The READ_IF_XOM_UNSUPPORTED
           // flag extends the execute permission with read options in this case.
           (s.executable() ? (ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED) : 0) |
           common_options;
  }

  // Allocate a contiguous address space region to hold all the segments and store
  // its handle in child_vmar_. The base address of the region is chosen by the
  // kernel, which can do ASLR. The memory_ member is updated to point to this
  // address, but it cannot be used until the actual mappings are in place.
  zx_status_t AllocateVmar(size_t vaddr_size, size_t vaddr_start);

  zx_status_t MapFileData(size_t segment_offset, bool is_writable, zx_vm_option_t options,
                          size_t map_size, zx::unowned_vmo vmo, std::string_view base_name,
                          size_t vmar_offset, size_t& num_mapped_file_regions);

  zx_status_t MapAnonymousData(zx_vm_option_t options, size_t zero_size, std::string_view base_name,
                               size_t vmar_offset, size_t& num_mapped_anonymous_regions);

  template <const std::string_view& Prefix>
  static void SetVmoName(zx::unowned_vmo vmo, std::string_view base_name, size_t n);

  cpp20::span<std::byte> image() { return memory_.image(); }

  DirectMemory memory_;

  // child_vmar_  the region of the address space that the object is being loaded into.
  // The child_vmar_ is initialized during loading and only cleared by either:
  // 1) committing the mapping to memory
  // 2) the destruction of the loader.
  //
  // The child_vmar_ must be preserved until commit because some Fuchsia syscalls that
  // act on VMARs, like modifying protections, cannot be applied through parent regions.
  zx::vmar child_vmar_;

  // This is the root VMAR that the mapping is placed into.
  zx::unowned_vmar vmar_;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_VMAR_LOADER_H_

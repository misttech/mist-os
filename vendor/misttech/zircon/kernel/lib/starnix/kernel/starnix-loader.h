// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_STARNIX_LOADER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_STARNIX_LOADER_H_

#include <lib/elfldltl/diagnostics.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/zircon.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/util/system.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#include <type_traits>
#include <utility>

#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <ktl/array.h>
#include <ktl/byte.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/span.h>

namespace starnix {

class StarnixLoader {
 public:
  // This can be specialized by the user for particular LoadInfo types passed
  // to Load().  It's constructed by Load() for each segment using the specific
  // LoadInfo::*Segment type, not the LoadInfo::Segment ktl::variant type, so
  // it can have a constructor that works differently for each type.  The
  // constructor will be called with the individual segment object reference,
  // and the VMO for the whole file.  Both will be valid for the lifetime of
  // the SegmentVmo object.  The handle returned by .vmo() will only be used
  // during that lifetime.  So, a specialization might return an object that
  // owns the VMO handle it yields.
  template <class LoadInfo>
  class SegmentVmo {
   public:
    SegmentVmo() = delete;

    template <class Segment>
    SegmentVmo(const Segment& segment, fbl::RefPtr<MemoryObject> memory)
        : memory_(ktl::move(memory)), offset_{segment.offset()} {}

    // This is the VMO to map the segment's contents from.
    fbl::RefPtr<MemoryObject> vmo() const { return memory_; }

    // If true and the segment is writable, make a copy-on-write child VMO.
    // This is the usual behavior when using the original file VMO directly.
    // A specialization can return anything convertible to bool.
    constexpr ktl::true_type copy_on_write() const { return {}; }

    // This is the offset within the VMO whence the segment should be mapped
    // (or cloned if this->copy_on_write()).
    uint64_t offset() const { return offset_; }

   private:
    fbl::RefPtr<MemoryObject> memory_;
    uint64_t offset_;
  };

  // When default-constructed, only the zx::vmar signature of Load can be used.
  // The default-constructed object can be assigned to another that has a
  // parent VMAR handle.
  StarnixLoader() = default;

  explicit StarnixLoader(fbl::RefPtr<MemoryManager> mm) : mm_(ktl::move(mm)) {}

  StarnixLoader(StarnixLoader&& other) = default;

  StarnixLoader& operator=(StarnixLoader&& other) = default;

  ~StarnixLoader() = default;

  [[gnu::const]] static size_t page_size() { return PAGE_SIZE; }

 protected:
  // This encapsulates the main differences between the Load methods of the
  // derived classes.  Each derived class's Load method just calls a different
  // VmarLoader::Load<Policy> instantiaton.  The PartialPagePolicy applies
  // specifically to LoadInfo::DataWithZeroFillSegment segments where the
  // segment's memsz > filesz and its vaddr + filesz is not page-aligned.
  enum class PartialPagePolicy {
    kProhibited,     // vaddr + filesz must be page-aligned.
    kCopyInProcess,  // zx_vmo_read the partial page into zero-fill memory.
    kZeroInVmo,      // ZX_VMO_OP_ZERO the remainder after a COW partial page.
  };

  // This loads the segments according to the elfldltl::LoadInfo<...>
  // instructions, mapping contents from the given VMO handle.  It's the real
  // initializer for the object, and other methods can only be used after it
  // returns success (true).  If it fails (returns false), the Diagnostics
  // object gets calls with the error details, and the VmarLoader object should
  // be destroyed promptly.
  //
  // After Load() returns, the caller must assume that the containing VMAR
  // passed to the constructor may have new mappings whether the call succeeded
  // or not. Any mappings made are cleared out by destruction of the VmarLoader
  // object unless Commit() is called, see below.
  template <PartialPagePolicy PartialPage, class Diagnostics, class LoadInfo>
  [[nodiscard]] bool Load(Diagnostics& diag, const LoadInfo& load_info,
                          fbl::RefPtr<MemoryObject> memory, size_t mapper_base, size_t vaddr_bias) {
    VmoName base_name_storage = GetVmoName(memory);
    ktl::string_view base_name = ktl::string_view(base_name_storage.data());

    auto mapper = [this, mapper_relative_bias = vaddr_bias - mapper_base, memory, &diag, base_name,
                   num_data_segments = size_t{0},
                   num_zero_segments = size_t{0}](const auto& segment) mutable {
      // Where in the VMAR the segment begins, accounting for load bias.
      const uintptr_t vmar_offset = segment.vaddr() + mapper_relative_bias;

      // Let the Segment type choose a different VMO and offset if it wants to.
      SegmentVmo<LoadInfo> segment_vmo{segment, memory};
      fbl::RefPtr<MemoryObject> map_vmo = segment_vmo.vmo();
      const auto map_cow = segment_vmo.copy_on_write();
      const uint64_t map_offset = segment_vmo.offset();

      // The read-only ConstantSegment shares little in common with the others.
      // The segment.writable() is statically true in the ConstantSegment
      // instantiation of the mapper call operator, so the rest of the code
      // will be compiled away.
      if (!segment.writable()) {
        // This is tautologically true in ConstantSegment.
        ZX_DEBUG_ASSERT(segment.filesz() == segment.memsz());

        zx_vm_option_t options = kVmCommon |  //
                                 (segment.readable() ? ZX_VM_PERM_READ : 0) |
                                 (segment.executable() ? kVmExecutable : 0);
        zx_status_t status = Map(vmar_offset, options, map_vmo, map_offset, segment.filesz());
        if (status != ZX_OK) [[unlikely]] {
          diag.SystemError("cannot map read-only segment", elfldltl::FileAddress{segment.vaddr()},
                           " from ", elfldltl::FileOffset{segment.offset()}, ": ",
                           elfldltl::ZirconError{status});
          return false;
        }

        return true;
      }

      // All the writable segment types can be handled as if they were the most
      // general case: DataWithZeroFillSegment.  This comprises up to three
      // regions, depending on the segment:
      //
      //   [file pages]* [intersecting page]? [anon pages]*
      //
      // The DataSegment and ZeroFillSegment cases are statically just the
      // first or the last, with no intersecting page.  The arithmetic and if
      // branches below will compile away in those instantiations of the mapper
      // lambda to calling just MapWritable or just MapZeroFill.
      //
      // * "file pages" are present when filesz > 0, 'map_size' below.
      // * "anon pages" are present when memsz > filesz, 'zero_size' below.
      // * "intersecting page" exists when both file pages and anon pages
      //   exist, and file pages are not an exact multiple of pagesize. At most
      //   a SINGLE intersecting page exists. It is represented by 'copy_size'
      //   below.
      //
      // IMPLEMENTATION NOTE: The VmarLoader performs only two mappings.
      //    * Mapping file pages up to the last full page of file data.
      //    * Mapping zero-fill pages, including the intersecting page, to the
      //    * end of the segment.
      //
      // TODO(https://fxbug.dev/42172744): Support mapping objects into VMAR from out of
      // process.
      //
      // After the second mapping, the VmarLoader then reads in the partial
      // file data into the intersecting page.
      //
      // The alternative would be to map filesz page rounded up into memory and
      // then zero out the zero fill portion of the intersecting page. This
      // isn't preferable because we would immediately cause a page fault and
      // spend time zero'ing a page when the OS may already have copied this
      // page for us.

      // This is tautologically zero in ZeroFillSegment.
      size_t map_size = segment.filesz();
      size_t zero_size = 0;
      size_t copy_size = 0;

      // This is tautologically false in DataSegment.  In ZeroFillSegment
      // map_size is statically zero so copy_size statically stays zero too.
      if (segment.memsz() > segment.filesz()) {
        switch (PartialPage) {
          case PartialPagePolicy::kProhibited:
            // MapWritable will assert that map_size is aligned.
            zero_size = segment.memsz() - map_size;
            break;
          case PartialPagePolicy::kCopyInProcess:
            // Round down what's mapped, and copy the difference.
            copy_size = map_size & (page_size() - 1);
            map_size &= -page_size();
            // Zero the remaining whole pages.
            zero_size = segment.memsz() - map_size;
            break;
          case PartialPagePolicy::kZeroInVmo:
            // MapWritable will round up what's mapped and zero the difference.
            // Just zero the remaining whole pages.
            zero_size = segment.memsz() - ((map_size + page_size() - 1) & -page_size());
            break;
        }
      }

      // First map data from the file, if any.
      if (map_size > 0) {
        zx_status_t status =
            MapWritable < PartialPage ==
            PartialPagePolicy::kZeroInVmo >
                (vmar_offset, map_vmo, map_cow, base_name, map_offset, map_size, num_data_segments);
        if (status != ZX_OK) [[unlikely]] {
          diag.SystemError("cannot map writable segment", elfldltl::FileAddress{segment.vaddr()},
                           " from file", elfldltl::FileOffset{map_offset}, ": ",
                           elfldltl::ZirconError{status});
          return false;
        }
      }

      // Then map zero-fill data, if any.
      if (zero_size > 0) {
        if constexpr (PartialPage == PartialPagePolicy::kZeroInVmo) {
          // MapWritable<ZeroInVmo=true> will have rounded up its true mapping
          // size and zeroed the trailing partial page.  So skip over that true
          // mapping size to the first page that's entirely zero-fill.
          map_size = (map_size + page_size() - 1) & -page_size();
        } else {
          // This should have been rounded down earlier.
          ZX_DEBUG_ASSERT((map_size & (page_size() - 1)) == 0);
        }
        const uintptr_t zero_fill_vmar_offset = vmar_offset + map_size;
        zx_status_t status =
            MapZeroFill(zero_fill_vmar_offset, base_name, zero_size, num_zero_segments);
        if (status != ZX_OK) [[unlikely]] {
          diag.SystemError("cannot map zero-fill pages",
                           elfldltl::FileAddress{segment.vaddr() + map_size}, ": ",
                           elfldltl::ZirconError{status});
          return false;
        }

        // Finally, copy the partial intersecting page, if any.
        if constexpr (PartialPage != PartialPagePolicy::kCopyInProcess) {
          ZX_DEBUG_ASSERT(copy_size == 0);
        } else if (copy_size > 0) {
#if 0
          const size_t copy_offset = map_offset + map_size;
          void* const copy_data =
              reinterpret_cast<void*>(mapper_relative_bias + zero_fill_vmar_offset + vaddr_bias);
          ktl::span<uint8_t> data((uint8_t*)copy_data, copy_size);
          auto result = map_vmo->read(data, copy_offset);
          if (result.is_error()) [[unlikely]] {
            diag.SystemError("cannot read segment data from file",
                             elfldltl::FileOffset{copy_offset}, ": ",
                             elfldltl::ZirconError{result.error_value()});
            return false;
          }
#endif
        }
      }
      return true;
    };

    return load_info.VisitSegments(mapper);
  }

 public:
  template <class Diagnostics, class LoadInfo>
  [[nodiscard]] bool map_elf_segments(Diagnostics& diag, const LoadInfo& load_info,
                                      fbl::RefPtr<MemoryObject> memory, size_t mapper_base,
                                      size_t vaddr_bias) {
    return Load<PartialPagePolicy::kZeroInVmo>(diag, load_info, memory, mapper_base, vaddr_bias);
  }

 private:
  using VmoName = ktl::array<char, ZX_MAX_NAME_LEN>;

  static VmoName GetVmoName(fbl::RefPtr<MemoryObject> memory);

  // Permissions on Fuchsia are trickier than Linux, so read these comments
  // carefully if you are encountering permission issues!
  //
  // * Every segment is mapped to a specific location in the child VMAR. The
  //    permission to do so is given by the ZX_VM_CAN_MAP_SPECIFIC
  //    permission on the fresh allocation.
  //
  // * ZX_VM_ALLOW_FAULTS is required by the kernel when mapping resizable
  //    or pager-backed VMOs, which we might be.

  static constexpr zx_vm_option_t kVmCommon = ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS;

  // Execute-only pages may or may not be available on the system.
  // The READ_IF_XOM_UNSUPPORTED gives the system license to make the
  // pages readable as well if execute-only was specified but can't
  // be honored.
  static constexpr zx_vm_option_t kVmExecutable =
      ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED;

  // Segment types other than ConstantSegment always use R/W permissions.
  static constexpr zx_vm_option_t kMapWritable = kVmCommon | ZX_VM_PERM_READ | ZX_VM_PERM_WRITE;

  // Allocate a contiguous address space region to hold all the segments and
  // store its handle in load_image_vmar_.  The base address of the region is
  // chosen by the kernel, which can do ASLR, unless specified by the optional
  // vmar_offset argument (relative to the parent VMAR given at construction).
  // The load_image_vmar_ and load_bias_ members are updated.
  // zx_status_t AllocateVmar(size_t vaddr_size, size_t vaddr_start,
  //                         ktl::optional<size_t> vmar_offset);

  zx_status_t Map(uintptr_t vmar_offset, zx_vm_option_t options, fbl::RefPtr<MemoryObject> memory,
                  uint64_t vmo_offset, size_t size) {
    auto status = memory->duplicate_handle(ZX_RIGHT_SAME_RIGHTS);
    if (status.is_error()) {
      return status.error_value();
    }
    fbl::RefPtr<MemoryObject> map_vmo = status.value();

    auto addr = mm_->base_addr.checked_add(vmar_offset);
    if (!addr.has_value()) {
      /*diag.SystemError("in elf load, addition overflow attempting to map at",
                       elfldltl::FileAddress{}, " + ", elfldltl::FileOffset{vmar_offset}, ": ",
                       elfldltl::ZirconError{status});*/
      return ZX_ERR_INVALID_ARGS;
    }

    auto result = mm_->map_memory({.type = DesiredAddressType::Fixed, .address = *addr}, map_vmo,
                                  vmo_offset, size, ProtectionFlagsImpl::from_vmar_flags(options),
                                  MappingOptionsFlags(MappingOptions::ELF_BINARY),
                                  {.type = MappingNameType::File});
    if (result.is_error()) {
      return ZX_ERR_INVALID_ARGS;
    }

    return ZX_OK;
  }

  template <bool ZeroPartialPage>
  zx_status_t MapWritable(uintptr_t vmar_offset, fbl::RefPtr<MemoryObject> vmo, bool copy_vmo,
                          ktl::string_view base_name, uint64_t vmo_offset, size_t size,
                          size_t& num_data_segments);

  zx_status_t MapZeroFill(uintptr_t vmar_offset, ktl::string_view base_name, size_t size,
                          size_t& num_zero_segments);

  // This is the root VMAR that the mapping is placed into.
  fbl::RefPtr<MemoryManager> mm_;
};

// Both possible MapWritable instantiations are defined in vmar-loader.cc.

extern template zx_status_t StarnixLoader::MapWritable<false>(  //
    uintptr_t vmar_offset, fbl::RefPtr<MemoryObject> vmo, bool copy_vmo, ktl::string_view base_name,
    uint64_t vmo_offset, size_t size, size_t& num_data_segments);

extern template zx_status_t StarnixLoader::MapWritable<true>(  //
    uintptr_t vmar_offset, fbl::RefPtr<MemoryObject> vmo, bool copy_vmo, ktl::string_view base_name,
    uint64_t vmo_offset, size_t size, size_t& num_data_segments);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_STARNIX_LOADER_H_

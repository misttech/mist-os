// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_ELF_LOADER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_ELF_LOADER_H_

#include <lib/elfldltl/layout.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/zircon.h>
#include <lib/fit/result.h>
#include <lib/mistos/util/bitflags.h>
#include <lib/process_builder/elf_parser.h>
#include <zircon/types.h>

#include <ranges>
#include <utility>

#include <ktl/pair.h>
#include <object/vm_object_dispatcher.h>

namespace process_builder {

using SegmentFlags = Flags<elfldltl::PhdrBase::Flags>;

enum class ErrorType : uint8_t {
  NothingToLoad,
  VmarAllocate,
  VmarMap,
  VmoCowClone,
  VmoCreate,
  VmoRead,
  VmoWrite,
  GetVmoName,
  SetVmoName,
};

class ElfLoadError {
 public:
  explicit ElfLoadError(ErrorType type, zx_status_t status = ZX_OK)
      : type_(type), status_(status) {}

  ErrorType type() const { return type_; }
  zx_status_t status() const { return status_; }

  // Returns an appropriate zx_status_t for the given error.
  zx_status_t as_zx_status() const {
    switch (type_) {
      case ErrorType::NothingToLoad:
        return ZX_ERR_NOT_FOUND;
      case ErrorType::VmarAllocate:
      case ErrorType::VmarMap:
      case ErrorType::VmoCowClone:
      case ErrorType::VmoCreate:
      case ErrorType::VmoRead:
      case ErrorType::VmoWrite:
      case ErrorType::GetVmoName:
      case ErrorType::SetVmoName:
        return status_;  // Return the associated zx_status_t
      default:
        return ZX_ERR_INTERNAL;  // Fallback for unknown errors
    }
  }

 private:
  ErrorType type_;
  zx_status_t status_;
};

/// Information on what an ELF requires of its address space.
struct LoadedElfInfo {
  /// The lowest address of the loaded ELF.
  size_t low;

  /// The highest address of the loaded ELF.
  size_t high;

  // SegmentFlags max_perm;
};

/// Returns the address space requirements to load this ELF. Attempting to load it into a VMAR with
/// permissions less than max_perm, or at a base such that the range [base+low, base+high] is not
/// entirely valid, will fail.
LoadedElfInfo loaded_elf_info(const Elf64Headers& headers);

/// Return value of load_elf.
struct LoadedElf {
  /// The VMAR that the ELF file was loaded into.
  fbl::RefPtr<VmAddressRegion> vmar;

  /// The virtual address of the VMAR.
  size_t vmar_base;

  /// The ELF entry point, adjusted for the base address of the VMAR.
  size_t entry;
};

class Mapper {
 public:
  virtual fit::result<zx_status_t, size_t> map(size_t vmar_offset,
                                               fbl::RefPtr<VmObjectDispatcher> vmo,
                                               uint64_t vmo_offset, size_t length,
                                               zx_vm_option_t flags) = 0;
};

// These must not be longer than ZX_MAX_NAME_LEN.
constexpr std::string_view kVmoNamePrefixData = "data:";
constexpr std::string_view kVmoNamePrefixBss = "bss:";

// prefix length must be less than ZX_MAX_NAME_LEN-1 and not contain any nul bytes.
template <const ktl::string_view& Prefix>
ktl::array<char, ZX_MAX_NAME_LEN> vmo_name_with_prefix(const ktl::string_view& name) {
  static_assert(Prefix.size() <= ZX_MAX_NAME_LEN - 1);

  if (name.empty()) {
    return vmo_name_with_prefix<Prefix>("<unknown ELF>");
  }

  ktl::array<char, ZX_MAX_NAME_LEN> buffer{};
  cpp20::span vmo_name(buffer.data(), ZX_MAX_NAME_LEN - 1);

  // First, "data" or "bss".
  size_t name_idx = Prefix.copy(vmo_name.data(), vmo_name.size());

  // Finally append the original VMO name, however much fits.
  cpp20::span avail = vmo_name.subspan(name_idx);
  name_idx += name.copy(avail.data(), avail.size());
  ZX_DEBUG_ASSERT(name_idx <= vmo_name.size());

  return buffer;
}

#if 0
class Loader {
 public:
  // This can be specialized by the user for particular LoadInfo types passed
  // to Load().  It's constructed by Load() for each segment using the specific
  // LoadInfo::*Segment type, not the LoadInfo::Segment std::variant type, so
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
    SegmentVmo(const Segment& segment, fbl::RefPtr<VmObjectDispatcher> vmo)
        : vmo_{std::move(vmo)}, offset_{segment.offset()} {}

    // This is the VMO to map the segment's contents from.
    fbl::RefPtr<VmObjectDispatcher> vmo() const { return vmo_; }

    // If true and the segment is writable, make a copy-on-write child VMO.
    // This is the usual behavior when using the original file VMO directly.
    // A specialization can return anything convertible to bool.
    constexpr std::true_type copy_on_write() const { return {}; }

    // This is the offset within the VMO whence the segment should be mapped
    // (or cloned if this->copy_on_write()).
    uint64_t offset() const { return offset_; }

   private:
    fbl::RefPtr<VmObjectDispatcher> vmo_;
    uint64_t offset_;
  };

  // When default-constructed, only the zx::vmar signature of Load can be used.
  // The default-constructed object can be assigned to another that has a
  // parent VMAR handle.
  explicit Loader(Mapper& mapper) : mapper_(mapper) {}

  // Loader(VmarLoader&& other) = default;

  // Loader& operator=(Loader&& other) = default;

  ~Loader() = default;

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
                          fbl::RefPtr<VmObjectDispatcher> vmo) {
    char vmo_name[ZX_MAX_NAME_LEN] = {};
    if (zx_status_t result = vmo->get_name(vmo_name); result != ZX_OK) {
      return false;
    }

    std::string_view base_name(vmo_name);

    auto mapper = [this, vaddr_start = load_info.vaddr_start(), vmo, &diag, base_name,
                   num_data_segments = size_t{0},
                   num_zero_segments = size_t{0}](const auto& segment) mutable {
      // Where in the VMAR the segment begins, accounting for load bias.
      const uintptr_t vmar_offset = segment.vaddr() - vaddr_start;

      // Let the Segment type choose a different VMO and offset if it wants to.
      SegmentVmo<LoadInfo> segment_vmo{segment, vmo};
      fbl::RefPtr<VmObjectDispatcher> map_vmo = segment_vmo.vmo();
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
        zx_status_t status =
            Map(vmar_offset, options, map_vmo->vmo(), map_offset, segment.filesz());
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
        }
#if 0
        else if (copy_size > 0) {
          const size_t copy_offset = map_offset + map_size;
          void* const copy_data =
              reinterpret_cast<void*>(vaddr_start + zero_fill_vmar_offset + load_bias_);
          zx_status_t read_status = map_vmo->Read(copy_data, copy_offset, copy_size);
          if (read_status != ZX_OK) [[unlikely]] {
            diag.SystemError("cannot read segment data from file",
                             elfldltl::FileOffset{copy_offset}, ": ",
                             elfldltl::ZirconError{status});
            return false;
          }
        }
#endif
      }
      return true;
    };

    return load_info.VisitSegments(mapper);
  }

 private:
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

  zx_status_t Map(uintptr_t vmar_offset, zx_vm_option_t options, const fbl::RefPtr<VmObjectDispatcher>& vmo,
                  uint64_t vmo_offset, size_t size) {
    auto result = mapper_.map(vmar_offset, vmo, vmo_offset, size, options);
    if (result.is_error()) {
      return result.error_value();
    }
    return ZX_OK;
  }

  template <bool ZeroPartialPage>
  zx_status_t MapWritable(uintptr_t vmar_offset, fbl::RefPtr<VmObjectDispatcher> vmo, bool copy_vmo,
                          std::string_view base_name, uint64_t vmo_offset, size_t size,
                          size_t& num_data_segments);

  zx_status_t MapZeroFill(uintptr_t vmar_offset, std::string_view base_name, size_t size,
                          size_t& num_zero_segments);

  Mapper& mapper_;
};

/// elfldltl::RemoteVmarLoader performs loading in any process, given a VMAR
/// (that process's root VMAR or a smaller region).  See VmarLoader above for
/// the primary API details.  RemoteVmarLoader works fine for the current
/// process too, but LocalVmarLoader may optimize that case better.
class RemoteLoader : public Loader {
 public:
  // Can be default-constructed or constructed with a const zx::vmar& argument.
  using Loader::Loader;

  template <class Diagnostics, class LoadInfo>
  [[nodiscard]] bool Load(Diagnostics& diag, const LoadInfo& load_info,
                          fbl::RefPtr<VmObjectDispatcher> vmo) {
    return Loader::Load<PartialPagePolicy::kZeroInVmo>(diag, load_info, vmo);
  }
};
#endif

/// Map the segments of an ELF into an existing VMAR.
fit::result<ElfLoadError> map_elf_segments(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                           Elf64Headers& headers, Mapper*, size_t mapper_base,
                                           size_t vaddr_bias);

}  // namespace process_builder

template <>
constexpr Flag<elfldltl::PhdrBase::Flags> Flags<elfldltl::PhdrBase::Flags>::FLAGS[] = {
    {elfldltl::PhdrBase::kExecute},
    {elfldltl::PhdrBase::kRead},
    {elfldltl::PhdrBase::kWrite},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_PROCESS_BUILDER_INCLUDE_LIB_PROCESS_BUILDER_ELF_LOADER_H_

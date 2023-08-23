// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SEGMENT_WITH_VMO_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SEGMENT_WITH_VMO_H_

#include <lib/fit/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>

#include "diagnostics.h"
#include "load.h"
#include "mapped-vmo-file.h"
#include "vmar-loader.h"
#include "zircon.h"

namespace elfldltl {

// elfldltl::SegmentWithVmo::Copy and elfldltl::SegmentWithVmo::NoCopy can be
// used as the SegmentWrapper template template parameter in elfldltl::LoadInfo
// instantiations.  Partial specializations of elfldltl::VmarLoader::SegmentVmo
// provided below will then check segment.vmo() when mapping a segment.  If
// it's a valid handle, then the segment is mapped from that VMO at offset 0;
// otherwise it's mapped from the main file VMO at segment.offset() as usual.
//
// The Copy version has the usual behavior of treating the VMO as read-only and
// making a copy-on-write child VMO for a writable segment.  The NoCopy version
// will instead map segment.vmo() itself writable, so it can only be used once
// as that VMO's contents will be modified by the process using it.
//
// The NoCopy case allows for simple out-of-process dynamic linking where
// nothing is cached for reuse.  The Copy case allows for the "zygote" model
// where VMOs can be reused to get copy-on-write sharing of segments where
// relocation has already been done.
//
// elfldltl::SegmentWithVmo::AlignSegments adjusts any LoadInfo segments that
// have a partial page of data followed by zero-fill bytes so that all segments
// can be mapped page-aligned without additional partial-page zeroing work (as
// elfldltl::AlignedRemoteVmarLoader requires).  To do this, it installs a
// per-segment VMO that copies (via copy-on-write snapshot) the data and then
// zero-fills the partial page directly in the VMO contents.  Using this with
// an elfldltl::SegmentWithVmo::NoCopy instantiation of elfldltl::LoadInfo
// really just does the zeroing work ahead of time; elfldltl::RemoteVmarLoader
// would do the same thing when mapping.  An elfldltl::SegmentWithVmo::Copy
// instantiation can be used either for a full zygote model where the
// relocations are applied in the stored VMOs, or simply to share the zeroing
// work across multiple separate loads with their own relocations applied to
// their own copies (this may be beneficial to overall page-sharing if the
// final data page didn't need to be touched for any relocations).
//
// elfldltl::SegmentWithVmo::MakeMutable can be applied to a particular segment
// to install its own copied VMO.  This is what out-of-process dynamic linking
// must do before applying relocations that touch that segment's contents.
//
// The elfldltl::SegmentWithVmo::GetMutableMemory class is meant for use with
// the elfldltl::LoadInfoMutableMemory adapter object.

class SegmentWithVmo {
 public:
  // This becomes an additional base type along with the original Segment type.
  struct VmoHolder {
    VmoHolder() = default;

    VmoHolder(VmoHolder&&) = default;

    explicit VmoHolder(zx::vmo vmo) : vmo_{std::move(vmo)} {}

    zx::vmo& vmo() { return vmo_; }
    const zx::vmo& vmo() const { return vmo_; }

   private:
    zx::vmo vmo_;
  };

  // The Copy and NoCopy templates are shorthands using this.
  template <class Copy>
  struct Wrapper {
    // Segment types that have a filesz() are replaced with this subclass.
    // ZeroFillSegment never needs a VMO handle.
    template <class Segment>
    struct WithVmo : public Segment, VmoHolder {
      using Segment::Segment;

      // Don't merge with any other segment if there's a VMO installed.  Its
      // contents won't cover the other segment.  Both segments will get the
      // reciprocal check, so if this segment has no VMO (yet) but the other
      // does, its CanMergeWith is the one to return false.
      template <class Other>
      bool CanMergeWith(const Other& other) const {
        return !vmo();
      }
    };

    // Use the wrapper if need be, or the original type if not.
    template <class Segment>
    using Type = std::conditional_t<kSegmentHasFilesz<Segment>, WithVmo<Segment>, Segment>;

    // This implements the VmarLoader::SegmentVmo class for LoadInfo types used
    // with SegmentWithVmo::Copy and SegmentWithVmo::NoCopy.  It's the base
    // class for the partial specialization of VmarLoader::SegmentVmo below.
    class SegmentVmo {
     public:
      SegmentVmo() = delete;

      template <class Segment>
      SegmentVmo(const Segment& segment, zx::unowned_vmo vmo)
          : vmo_{vmo->borrow()}, offset_{segment.offset()} {
        if constexpr (kSegmentHasFilesz<Segment>) {
          // This is not a ZeroFillSegment, so it might have a VMO stored.
          if (segment.vmo()) {
            // This VMO at offset 0 replaces the file VMO at segment.offset().
            vmo_ = segment.vmo().borrow();
            offset_ = 0;

            // In SegmentWithVmo::NoCopy, copy_on_write() is true only when
            // using the file VMO, not the stored VMO.
            copy_on_write_ = CopyFalse{};
          }
        }
      }

      zx::unowned_vmo vmo() const { return vmo_->borrow(); }

      constexpr auto copy_on_write() const { return copy_on_write_; }

      uint64_t offset() const { return offset_; }

     private:
      // In Copy, copy_on_write() is statically true all the time.  In NoCopy,
      // it's true by default but can be set to false in the constructor.
      using CopyTrue = std::conditional_t<Copy{}, Copy, std::true_type>;
      using CopyFalse = std::conditional_t<Copy{}, Copy, std::false_type>;

      zx::unowned_vmo vmo_;
      std::conditional_t<Copy{}, Copy, bool> copy_on_write_{CopyTrue{}};
      uint64_t offset_;
    };
  };

  template <class Segment>
  using Copy = typename Wrapper<std::true_type>::template Type<Segment>;

  template <class Segment>
  using NoCopy = Wrapper<std::false_type>::template Type<Segment>;

  // This takes a LoadInfo::*Segment type that has file contents (i.e. not
  // ZeroFillSegment), and ensures that segment.vmo() is a valid segment.
  // Unless there's a VMO handle there already, it creates a new copy-on-write
  // child VMO from the original file VMO.
  template <class Diagnostics, class Segment>
  static bool MakeMutable(Diagnostics& diag, Segment& segment, zx::unowned_vmo vmo) {
    if (!segment.vmo()) {
      zx_status_t status =
          CopyVmo(vmo->borrow(), segment.offset(), segment.filesz(), segment.vmo());
      if (status != ZX_OK) [[unlikely]] {
        return SystemError(diag, segment, kCopyVmoFail, status);
      }
    }
    return true;
  }

  // elfldltl::SegmentWithVmo::AlignSegments modifies a LoadInfo that uses the
  // SegmentWithVmo wrappers.  It ensures that all segment.filesz() values are
  // page-aligned, so that elfldltl::AlignedRemoteVmarLoader can be used later.
  // To start with, each segment must have no vmo() handle installed.  If a
  // segment has a misaligned filesz(), then a new copy-on-write child of the
  // main file VMO is installed as its vmo().  In the modified VMO, the partial
  // page has been cleared to all zero bytes.  The segment.filesz() has been
  // rounded up to whole pages so it can be mapped with no additional zeroing.
  template <class Diagnostics, class LoadInfo>
  static bool AlignSegments(Diagnostics& diag, LoadInfo& info, zx::unowned_vmo vmo,
                            size_t page_size) {
    using DataWithZeroFillSegment = typename LoadInfo::DataWithZeroFillSegment;
    return info.VisitSegments([vmo, page_size, &diag](auto& segment) {
      using Segment = std::decay_t<decltype(segment)>;
      if constexpr (std::is_same_v<Segment, DataWithZeroFillSegment>) {
        const size_t zero_size = segment.MakeAligned(page_size);
        if (zero_size > 0) {
          ZX_DEBUG_ASSERT(!segment.vmo());
          if (!MakeMutable(diag, segment, vmo->borrow())) [[unlikely]] {
            return false;
          }
          const uint64_t zero_offset = segment.filesz() - zero_size;
          zx_status_t status = ZeroVmo(segment.vmo().borrow(), zero_offset, zero_size);
          if (status != ZX_OK) [[unlikely]] {
            return SystemError(diag, segment, kZeroVmoFail, status);
          }
        }
      }
      return true;
    });
  }

  // elfldltl::SegmentWithVmo::GetMutableMemory must be instantiated with a
  // LoadInfo type using SegmentWithVmo wrapper types, and created with a
  // zx::unowned_vmo for the original file contents.  It can then be used to
  // construct an elfldltl::LoadInfoMutableMemory adapter object (see details
  // in <lib/elfldltl/loadinfo-mutable-memory.h>), which provides the mutation
  // calls in the Memory API.  When mutation requests are made on that Memory
  // object, the GetMutableMemory callback will be used to map in the mutable
  // VMO for the segment contents.  The mutable VMO is created via MakeMutable
  // if needed, and then stored in the LoadInfo::Segment object for later use.
  //
  // The callable object itself is copyable and default-constructible
  // (requiring later copy-assignment to be usable).  Note that it holds (and
  // copies) the unowned handles passed in the constructor, so users are
  // responsible for ensuring those handles remain valid for the lifetime of
  // the callable object.  The optional second argument to the constructor is a
  // zx::unowned_vmar used for making mappings, default zx::vmar::root_self().
  template <class LoadInfo>
  class GetMutableMemory {
   public:
    using Result = fit::result<bool, MappedVmoFile>;
    using Segment = typename LoadInfo::Segment;

    GetMutableMemory() = default;

    GetMutableMemory(const GetMutableMemory&) = default;

    explicit GetMutableMemory(zx::unowned_vmo vmo, zx::unowned_vmar vmar = zx::vmar::root_self())
        : vmo_{vmo->borrow()}, vmar_{vmar->borrow()} {}

    GetMutableMemory& operator=(const GetMutableMemory&) = default;

    template <class Diagnostics>
    Result operator()(Diagnostics& diag, Segment& segment) const {
      auto get_memory = [this, &diag](auto& segment) -> Result {
        using SegmentType = std::decay_t<decltype(segment)>;
        if constexpr (kSegmentHasFilesz<SegmentType>) {
          // Make sure there's a mutable VMO available.
          if (!MakeMutable(diag, segment, vmo_->borrow())) [[unlikely]] {
            return fit::error(false);
          }
          if (!segment.vmo()) [[unlikely]] {
            // MakeMutable failed but Diagnostics said to keep going.
            // No sense also reporting the failure to map the invalid handle.
            return fit::error{true};
          }

          // Now map it in for mutation.
          MappedVmoFile memory;
          if (auto result = memory.InitMutable(segment.vmo().borrow(), segment.filesz(),
                                               segment.vaddr(), vmar_->borrow());
              result.is_error()) {
            return fit::error{SystemError(diag, segment, kMapFail, result.error_value())};
          }

          return fit::ok(std::move(memory));
        } else {
          // This should be impossible via LoadInfoMutableMemory.
          return fit::error{diag.FormatError(kMutableZeroFill)};
        }
      };
      return std::visit(get_memory, segment);
    }

   private:
    zx::unowned_vmo vmo_;
    zx::unowned_vmar vmar_;
  };

 private:
  static constexpr std::string_view kCopyVmoFail =
      "cannot create copy-on-write VMO for segment contents";
  static constexpr std::string_view kZeroVmoFail =
      "cannot zero partial page in VMO for data segment";
  static constexpr std::string_view kColonSpace = ": ";
  static constexpr std::string_view kMapFail = "cannot map segment to apply relocations";
  static constexpr std::string_view kMutableZeroFill = "cannot make zero-fill segment mutable";

  template <class Diagnostics, class Segment>
  static constexpr bool SystemError(Diagnostics& diag, const Segment& segment,
                                    std::string_view fail, zx_status_t status) {
    return diag.SystemError(fail, FileOffset{segment.offset()}, kColonSpace, ZirconError{status});
  }

  static zx_status_t CopyVmo(zx::unowned_vmo vmo, uint64_t offset, uint64_t size,
                             zx::vmo& segment_vmo) {
    return vmo->create_child(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, offset, size, &segment_vmo);
  }

  static zx_status_t ZeroVmo(zx::unowned_vmo segment_vmo, uint64_t offset, uint64_t size) {
    return segment_vmo->op_range(ZX_VMO_OP_ZERO, offset, size, nullptr, 0);
  }
};

// These are the SegmentVmo types used for LoadInfo<..., SegmentWithVmo::...>.
// They use a segment.vmo() if it's present.
//
// Note that a specialization using a template template parameter matches only
// that exact parameter, not even a template alias to the same thing.  So two
// separate partial specializations are required here to match all LoadInfo
// instantiations using either SegmentWithVmo::... template.

template <class Elf, template <class> class Container, PhdrLoadPolicy Policy>
class VmarLoader::SegmentVmo<LoadInfo<Elf, Container, Policy, SegmentWithVmo::Copy>>
    : public SegmentWithVmo::Wrapper<std::true_type>::SegmentVmo {
  using SegmentWithVmo::Wrapper<std::true_type>::SegmentVmo::SegmentVmo;
};

template <class Elf, template <class> class Container, PhdrLoadPolicy Policy>
class VmarLoader::SegmentVmo<LoadInfo<Elf, Container, Policy, SegmentWithVmo::NoCopy>>
    : public SegmentWithVmo::Wrapper<std::false_type>::SegmentVmo {
  using SegmentWithVmo::Wrapper<std::false_type>::SegmentVmo::SegmentVmo;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_SEGMENT_WITH_VMO_H_

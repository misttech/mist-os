// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_DECODED_MODULE_IN_MEMORY_H_
#define LIB_LD_DECODED_MODULE_IN_MEMORY_H_

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/soname.h>
#include <lib/elfldltl/static-vector.h>

#include "decoded-module.h"
#include "load.h"

namespace ld {

// This is a subclass of ld::DecodedModule specifically for decoding a module
// by loading it with a Loader API object (see <lib/elfldltl/mmap-loader.h> and
// <lib/elfldltl/vmar-loader.h>), and then decoding its metadata segments in
// place in the current process's own memory.
//
// The LoadFromFile method uses the Loader and File template APIs to populate
// load_info() and the vaddr / phdrs parts of module().

template <class ElfLayout = elfldltl::Elf<>,
          template <typename> class SegmentContainer =
              elfldltl::StaticVector<kMaxSegments>::Container,
          AbiModuleInline InlineModule = AbiModuleInline::kNo,
          DecodedModuleRelocInfo WithRelocInfo = DecodedModuleRelocInfo::kYes,
          template <class SegmentType> class SegmentWrapper = elfldltl::NoSegmentWrapper>
class DecodedModuleInMemory : public DecodedModule<ElfLayout, SegmentContainer, InlineModule,
                                                   WithRelocInfo, SegmentWrapper> {
 public:
  using Base =
      DecodedModule<ElfLayout, SegmentContainer, InlineModule, WithRelocInfo, SegmentWrapper>;

  using typename Base::Ehdr;
  using typename Base::Elf;
  using typename Base::LoadInfo;
  using typename Base::Module;
  using typename Base::Phdr;
  using Region = typename LoadInfo::Region;

  // This uses Loader API and File API objects to get the file's memory image
  // set up, which populates load_info().  This must be called after
  // HasModule() is true, so it can initialize the module() fields for phdrs
  // and vaddr bounds.  It returns the elfldltl::LoadHeadersFromFile return
  // value, so the ehdr and phdrs can be examined further.  (Note that
  // module().phdrs might validly be empty, so the returned phdrs buffer should
  // be used for further decoding.)  The File object should no longer be needed
  // after this, so it can be passed as an rvalue and consumed if convenient.
  // As long as the PhdrAllocator object does not own the allocations it
  // returns, then it can be a consumed rvalue too.
  template <class Diagnostics, class Loader, class File,
            typename PhdrAllocator = elfldltl::FixedArrayFromFile<Phdr, kMaxPhdrs>>
  constexpr auto LoadFromFile(Diagnostics& diag, Loader& loader, File&& file,
                              PhdrAllocator&& phdr_allocator = PhdrAllocator{})
      -> decltype(elfldltl::LoadHeadersFromFile<Elf>(  //
          diag, file, std::forward<PhdrAllocator>(phdr_allocator))) {
    assert(this->HasModule());

    // Read the file header and program headers into stack buffers.
    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, file, std::forward<PhdrAllocator>(phdr_allocator));
    if (headers) [[likely]] {
      auto& [ehdr_owner, phdrs_owner] = *headers;
      const Ehdr& ehdr = ehdr_owner;
      const cpp20::span<const Phdr> phdrs = phdrs_owner;

      // Decode phdrs just to fill load_info().  There will need to be another
      // pass over the phdrs after the image is mapped in, because metadata
      // segments like notes refer to data in memory and it's not there yet.
      // So everything else can wait until then.  With that, load_info() has
      // enough information to actually load the file.  Once the segments are
      // in all memory, then a metadata phdr can be decoded via its vaddr.
      if (elfldltl::DecodePhdrs(diag, phdrs,
                                this->load_info().GetPhdrObserver(loader.page_size())) &&
          loader.Load(diag, this->load_info(), file.borrow())) [[likely]] {
        // Update the module to reflect the runtime vaddr bounds.
        SetModuleVaddrBounds(this->module(), this->load_info(), loader.load_bias());

        // Update the module to point to the phdrs in memory.  Note that the
        // rest of the decoding still uses the original phdrs buffer in the
        // return value just in case the phdrs aren't in the memory image.
        SetModulePhdrs(this->module(), ehdr, this->load_info(), loader.memory());

        return headers;
      }
    }

    return std::nullopt;
  }
};

}  // namespace ld

#endif  // LIB_LD_DECODED_MODULE_IN_MEMORY_H_

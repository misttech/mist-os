// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_REMOTE_DECODED_MODULE_H_
#define LIB_LD_REMOTE_DECODED_MODULE_H_

#include <lib/elfldltl/container.h>
#include <lib/elfldltl/load.h>
#include <lib/elfldltl/loadinfo-mapped-memory.h>
#include <lib/elfldltl/mapped-vmo-file.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/relocation.h>
#include <lib/elfldltl/segment-with-vmo.h>
#include <lib/elfldltl/soname.h>
#include <lib/fit/result.h>
#include <lib/ld/load-module.h>
#include <lib/ld/load.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

namespace ld {

// ld::RemoteDecodedModule represents an ELF file and all the metadata
// extracted from it.  It's specifically meant only to hold a cache of
// information distilled purely from the file's contents.  So it doesn't
// include a name, runtime load address, symbolizer module ID, or TLS module
// ID.  The tls_module_id() method returns 1 if the module has a PT_TLS at all.
//
// The RemoteDecodedModule object owns a read-and-execute-only VMO handle for
// the file's immutable contents and a mapping covering all its segments
// (perhaps the whole file).  The VMO is supplied at construction and is owned
// for the lifetime of the RemoteDecodedModule.  The Init method decodes the
// ELF file's metadata and prepares the RemoteDecodedModule for use.  All other
// methods are const.
//
// If Init encountered errors then the object may be in a partially-initialized
// state where HasModule() returns false, or where it returns true but the
// mapped_vmo() and/or module() and/or load_info() data is incomplete.  How
// much partial work might be done (and the return value of Init) depends on
// when the Diagnostics object says to keep going.  An incomplete object that
// won't be used should be destroyed because it may use substantial resources
// (like mapping the whole file VMO into the local address space).
//
// It's a movable object, but moving it does not invalidate all the metadata
// pointers.  For the lifetime of the RemoteDecodedModule, other objects can
// point into the mapped file's metadata such as by doing shallow copies of
// `.module()`.  The `.load_info()` object may own move-only zx::vmo handles to
// VMOs in `.segments()` via elfldltl::SegmentWithVmo::Copy.  (The distinction
// between NoCopy and Copy doesn't really matter here, since the segments in
// RemoteDecodedModule should never be passed to a VmarLoader.  Using Copy just
// expresses the abstract intent that RemoteDecodedModule be used in a const
// fashion, including never modifying contents of VMOs it owns after Init.)  As
// no relocations are performed on these segments, such a VMO will only exist
// when a DataWithZeroFillSegment with a partial page of bss is adjusted by
// elfldltl::SegmentWithVmo::AlignSegments with a separate VMO.  Any new VMO
// becomes immutable (with no ZX_RIGHT_WRITE on the only handle) once its final
// partial page has been zeroed.

// This is a shorthand for the <lib/elfldltl/container.h> wrappers used here.
template <typename T>
using RemoteContainer = elfldltl::StdContainer<std::vector>::Container<T>;

// This is an implementation detail of RemoteDecodedModule, below.
template <class Elf>
using RemoteDecodedModuleBase =
    DecodedModule<Elf, RemoteContainer, AbiModuleInline::kYes, DecodedModuleRelocInfo::kYes,
                  elfldltl::SegmentWithVmo::Copy>;

template <class Elf = elfldltl::Elf<>>
class RemoteDecodedModule : public RemoteDecodedModuleBase<Elf>,
                            public fbl::RefCounted<RemoteDecodedModule<Elf>> {
 public:
  // ld::RemoteDecodedModule is usually used only via const pointer.
  // Only the Init method is called on a mutable ld::RemoteDecodedModule.
  using Ptr = fbl::RefPtr<const RemoteDecodedModule>;

  using Base = RemoteDecodedModuleBase<Elf>;
  static_assert(std::is_move_constructible_v<Base>);
  static_assert(std::is_move_assignable_v<Base>);

  using typename Base::LoadInfo;
  using typename Base::Phdr;
  using typename Base::size_type;
  using typename Base::Soname;
  using Ehdr = typename Elf::Ehdr;

  // Names of each DT_NEEDED entry for the module.
  using NeededList = std::vector<Soname>;

  // Information from decoding a main executable, specifically.  This
  // information may exist in any file, but it's only of interest when
  // launching a main executable.
  struct ExecInfo {
    size_type relative_entry = 0;         // File-relative entry point address.
    std::optional<size_type> stack_size;  // Any requested initial stack size.
  };

  // This is the Memory API object returned by memory_metadata(), below.
  using MetadataMemory = elfldltl::LoadInfoMappedMemory<LoadInfo, elfldltl::MappedVmoFile>;

  // A default-constructed object is just an empty placeholder that can be
  // move-assigned.  An empty object (where `!this->vmo()`) could be used as a
  // negative cache entry in a file identity -> RemoteDecodedModule map without
  // holding onto a VMO handle for the invalid file.
  RemoteDecodedModule() = default;

  // RemoteDecodedModule is move-constructible and move-assignable.
  RemoteDecodedModule(RemoteDecodedModule&&) = default;

  // After construction, Init should be called to do the actual decoding.
  explicit RemoteDecodedModule(zx::vmo vmo) : vmo_(std::move(vmo)) {}

  RemoteDecodedModule& operator=(RemoteDecodedModule&&) = default;

  // The VMO can be used or borrowed during the lifetime of this object.
  // Before Init, this is the only method that will return non-empty data.
  const zx::vmo& vmo() const { return vmo_; }

  // After Init, this is the File API object with the file's contents.
  const elfldltl::MappedVmoFile& mapped_vmo() const { return mapped_vmo_; }

  // After Init, this has the information relevant for a main executable.
  const ExecInfo& exec_info() const { return exec_info_; }

  // After Init, this reports the e_machine field.  Decoding does not care
  // which machine it's for, but consuming it likely will.
  elfldltl::ElfMachine machine() const { return machine_; }

  // After Init, this is the list of direct DT_NEEDED dependencies in this
  // object.  Each element's .str() / .c_str() pointers point into the mapped
  // file image and are valid for the lifetime of this RemoteDecodedModule (or
  // until it's assigned).
  const NeededList& needed() const { return needed_; }

  // This creates and initializes a new RemoteDecodedModule from a VMO.  See
  // Init() below for details about interaction with the Diagnostics object.
  // This returns a null pointer if Init() returned false.  In all cases, the
  // VMO handle is consumed.
  template <class Diagnostics>
  static Ptr Create(Diagnostics& diag, zx::vmo vmo, size_type page_size) {
    auto decoded = fbl::MakeRefCounted<RemoteDecodedModule>(std::move(vmo));
    if (!decoded->Init(diag, page_size)) {
      decoded.reset();
    }
    return decoded;
  }

  // Initialize the module from the provided VMO, representing either the
  // binary or shared library to be loaded.  Create the data structures that
  // make the VMO readable, and scan and decode its phdrs to set and return
  // relevant information about the module to make it ready for relocation and
  // loading.  If the Diagnostics object says to keep going, the module may be
  // uninitialilzed such that HasModule() is false or there is partial
  // information.  This could be used as negative caching for files that have
  // already been examined and found to be invalid.
  template <class Diagnostics>
  bool Init(Diagnostics& diag, size_type page_size) {
    if (auto status = mapped_vmo_.Init(vmo_.borrow()); status.is_error()) {
      // Return true if the Diagnostics object did too, but there is no way to
      // keep going if the file data didn't get mapped in.
      return diag.SystemError("cannot map VMO file", elfldltl::ZirconError{status.status_value()});
    }

    // Get direct pointers to the file header and the program headers inside
    // the mapped file image.
    constexpr elfldltl::NoArrayFromFile<Phdr> kNoPhdrAllocator;
    auto headers =
        elfldltl::LoadHeadersFromFile<Elf>(diag, mapped_vmo_, kNoPhdrAllocator, std::nullopt);
    if (!headers) [[unlikely]] {
      // TODO(mcgrathr): LoadHeadersFromFile doesn't propagate Diagnostics
      // return value on failure.
      return false;
    }

    // Instantiate the module so we can start to set its fields.
    // The symbolizer_modid is not meaningful here.
    this->EmplaceModule(0);

    // Decode phdrs to fill LoadInfo, build ID, etc.  Only one pass over the
    // phdrs is needed since metadata segments can be accessed by offset rather
    // than vaddr, such as via the PhdrFileNoteObserver.
    auto& [ehdr_owner, phdrs_owner] = *headers;
    const Ehdr& ehdr = ehdr_owner;
    const std::span<const Phdr> phdrs = phdrs_owner;
    constexpr elfldltl::NoArrayFromFile<std::byte> kNoBuildIdAllocator;
    auto result = DecodeModulePhdrs<Elf>(  //
        diag, phdrs, this->load_info().GetPhdrObserver(page_size),
        PhdrFileBuildIdObserver<Elf>(mapped_vmo_, kNoBuildIdAllocator, this->module()));
    if (!result) [[unlikely]] {
      // DecodeModulePhdrs only fails if Diagnostics said to give up.
      return false;
    }

    auto [dyn_phdr, tls_phdr, relro_phdr, stack_size] = *result;

    exec_info_ = {.relative_entry = ehdr.entry, .stack_size = stack_size};
    machine_ = ehdr.machine;

    // Apply RELRO protection before segments are aligned & equipped with VMOs.
    if (!this->load_info().ApplyRelro(diag, relro_phdr, page_size, false)) [[unlikely]] {
      // ApplyRelro only fails if Diagnostics said to give up.
      return false;
    }

    // Fix up segments to be compatible with AlignedRemoteVmarLoader.  Any
    // per-segment VMOs created for partial-page zeroing become immutable.
    // Only copy-on-write clones of them will have relocations or other
    // mutations applied or be mapped writable in any process.
    if (!elfldltl::SegmentWithVmo::AlignSegments(diag, this->load_info(), vmo_.borrow(), page_size,
                                                 true)) [[unlikely]] {
      // AlignSegments only fails if Diagnostics said to give up.
      return false;
    }

    auto memory = metadata_memory();
    SetModulePhdrs<Elf>(this->module(), ehdr, this->load_info(), memory);

    // If there was a PT_TLS, fill in tls_module() to be published later.
    // The TLS module ID is not meaningful here, it just has to be nonzero.
    if (tls_phdr) {
      this->SetTls(diag, memory, *tls_phdr, 1);
    }

    // Decode everything else from the PT_DYNAMIC data.  Each DT_NEEDED has an
    // offset into the DT_STRTAB, but the single pass finds DT_STRTAB and sees
    // each DT_NEEDED at the same time.  So the NeededObserver just collects
    // their offsets and then those are reified into strings afterwards.
    RemoteContainer<size_type> needed_offsets;
    if (auto result =
            this->DecodeDynamic(diag, memory, dyn_phdr, Base::MakeNeededObserver(needed_offsets));
        result.is_error()) [[unlikely]] {
      return result.error_value();
    }

    // Now that DT_STRTAB has been decoded, it's possible to reify each offset
    // into the corresponding SONAME string (and hash it by creating a Soname).
    std::optional needed_names = this->template ReifyNeeded<RemoteContainer>(diag, needed_offsets);
    if (!needed_names) [[unlikely]] {
      return false;
    }
    needed_ = *std::move(needed_names);

    return true;
  }

  // Create and return a memory-adaptor object that serves as a wrapper around
  // this module's LoadInfo and MappedVmoFile.  This is used to translate
  // vaddrs into file-relative offsets in order to read from the VMO.
  MetadataMemory metadata_memory() const {
    return MetadataMemory{
        this->load_info(),
        // The DirectMemory API expects a mutable *this just because it's the
        // API exemplar and toolkit pieces shouldn't presume a Memory API
        // object is usable as const&.  But MappedVmoFile in fact is all const
        // after Init.
        const_cast<elfldltl::MappedVmoFile&>(mapped_vmo_),
    };
  }

 private:
  elfldltl::MappedVmoFile mapped_vmo_;
  NeededList needed_;
  ExecInfo exec_info_;
  zx::vmo vmo_;
  elfldltl::ElfMachine machine_ = elfldltl::ElfMachine::kNone;
};

}  // namespace ld

#endif  // LIB_LD_REMOTE_DECODED_MODULE_H_

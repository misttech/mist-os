// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/process_builder/elf_loader.h"

#include <lib/elfldltl/load.h>
#include <lib/elfldltl/memory.h>
#include <lib/elfldltl/phdr.h>
#include <lib/elfldltl/static-vector.h>
#include <lib/process_builder/elf_parser.h>
#include <lib/process_builder/util.h>

#include "arch/defines.h"
#include "lib/fit/result.h"

namespace process_builder {

namespace {

auto GetDiagnostics() {
  return elfldltl::Diagnostics(elfldltl::PrintfDiagnosticsReport(
                                   [](auto&&... args) {
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wformat-nonliteral"
                                     printf(std::forward<decltype(args)>(args)...);
                                     printf("\n");
#pragma GCC diagnostic pop
                                   },
                                   "elf loader: "),
                               elfldltl::DiagnosticsPanicFlags());
}

}  // namespace

LoadedElfInfo loaded_elf_info(const Elf64Headers& headers) {
  // uint32_t max_perm;
  ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr> phdrs = headers.program_headers();
  elfldltl::LoadInfo<elfldltl::Elf<elfldltl::ElfClass::k64>,
                     elfldltl::StaticVector<kMaxSegments>::Container>
      load_info;
  ZX_ASSERT(elfldltl::DecodePhdrs(GetDiagnostics(), phdrs, load_info.GetPhdrObserver(PAGE_SIZE)));

  /*load_info.VisitSegments([&max_perm](const auto& segment) {
    max_perm |= segment.readable() ? elfldltl::PhdrBase::kRead : 0;
    max_perm |= segment.writable() ? elfldltl::PhdrBase::kWrite : 0;
    max_perm |= segment.executable() ? elfldltl::PhdrBase::kExecute : 0;
    return true;
  });*/

  return LoadedElfInfo{.low = load_info.vaddr_start(),
                       .high = (load_info.vaddr_start() + load_info.vaddr_size())};
}

#if 0
// This has both explicit instantiations below.
template <bool ZeroInVmo>
zx_status_t Loader::MapWritable(uintptr_t vmar_offset, fbl::RefPtr<VmObjectDispatcher> vmo,
                                bool copy_vmo, std::string_view base_name, uint64_t vmo_offset,
                                size_t size, size_t& num_data_segments) {
  if constexpr (!ZeroInVmo) {
    ZX_DEBUG_ASSERT((size & (page_size() - 1)) == 0);
  }

  fbl::RefPtr<VmObjectDispatcher> map_vmo;
  if (copy_vmo) {
    fbl::RefPtr<VmObject> writeable_vmo;
    zx_status_t status = vmo->CreateChild(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, vmo_offset, size,
                                          false, &writeable_vmo);
    if (status != ZX_OK) [[unlikely]] {
      return status;
    }
    KernelHandle<VmObjectDispatcher> handle;
    zx_rights_t rights;
    if (status = VmObjectDispatcher::Create(ktl::move(writeable_vmo), PAGE_SIZE * 2ul,
                                            VmObjectDispatcher::InitialMutability::kMutable,
                                            &handle, &rights);
        status != ZX_OK) {
      return status;
    }
    map_vmo = handle.release();
  } else {
    map_vmo = vmo;
  }

  // If the size is not page-aligned, zero the last page beyond the size.
  if constexpr (ZeroInVmo) {
    const size_t subpage_size = size & (page_size() - 1);
    if (subpage_size > 0) {
      const size_t zero_offset = size;
      const size_t zero_size = page_size() - subpage_size;
      zx_status_t status = map_vmo->vmo()->ZeroRange(zero_offset, zero_size);
      if (status != ZX_OK) [[unlikely]] {
        return status;
      }
    }
  }

  // SetVmoName<kVmoNamePrefixData>(map_vmo->borrow(), base_name, num_data_segments++);
  auto name = vmo_name_with_prefix<kVmoNamePrefixData>(base_name);
  if (zx_status_t result = map_vmo->set_name(name.data(), name.size()); result != ZX_OK) {
    return result;
  }

  return Map(vmar_offset, kMapWritable, map_vmo, 0, size);
}

// Explicitly instantiate both flavors.

template zx_status_t Loader::MapWritable<false>(  //
    uintptr_t vmar_offset, fbl::RefPtr<VmObjectDispatcher> vmo, bool copy_vmo,
    std::string_view base_name, uint64_t vmo_offset, size_t size, size_t& num_data_segments);

template zx_status_t Loader::MapWritable<true>(  //
    uintptr_t vmar_offset, fbl::RefPtr<VmObjectDispatcher> vmo, bool copy_vmo,
    std::string_view base_name, uint64_t vmo_offset, size_t size, size_t& num_data_segments);

zx_status_t Loader::MapZeroFill(uintptr_t vmar_offset, std::string_view base_name, size_t size,
                                size_t& num_zero_segments) {
  zx::result<VmObjectDispatcher::CreateStats> parse_result =
      VmObjectDispatcher::parse_create_syscall_flags(0, size);
  if (parse_result.is_error()) {
    // return fit::error(parse_result.error_value());
    return false;
  }
  VmObjectDispatcher::CreateStats stats = parse_result.value();

  fbl::RefPtr<VmObjectPaged> vmo;
  if (zx_status_t status = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, stats.flags, stats.size, &vmo);
      status != ZX_OK) [[unlikely]]
  {
    return status;
  }
  KernelHandle<VmObjectDispatcher> handle;
  zx_rights_t rights;
  if (zx_status_t status = VmObjectDispatcher::Create(
          ktl::move(vmo), PAGE_SIZE * 2ul, VmObjectDispatcher::InitialMutability::kMutable, &handle,
          &rights);
      status != ZX_OK) {
    return status;
  }

  auto name = vmo_name_with_prefix<kVmoNamePrefixBss>(base_name);
  if (zx_status_t result = vmo->set_name(name.data(), name.size()); result != ZX_OK) {
    return result;
  }

  return Map(vmar_offset, kMapWritable, handle.release(), 0, size);
}
#endif

fit::result<ElfLoadError> map_elf_segments(const fbl::RefPtr<VmObjectDispatcher>& vmo,
                                           Elf64Headers& headers, Mapper* mapper,
                                           size_t mapper_base, size_t vaddr_bias) {
  auto diag = GetDiagnostics();
  ktl::span<const elfldltl::Elf<elfldltl::ElfClass::k64>::Phdr> phdrs = headers.program_headers();
  elfldltl::LoadInfo<elfldltl::Elf<elfldltl::ElfClass::k64>,
                     elfldltl::StaticVector<kMaxSegments>::Container>
      load_info;
  ZX_ASSERT(elfldltl::DecodePhdrs(diag, phdrs, load_info.GetPhdrObserver(PAGE_SIZE)));

  // We intentionally use wrapping subtraction here, in case the ELF file happens to use vaddr's
  // that are higher than the VMAR base chosen by the kernel. Wrapping addition will be used when
  // adding this bias to vaddr values.
  // TODO (Herrera): wrapping_sub
  auto mapper_relative_bias = vaddr_bias - mapper_base;

  char vmo_name[ZX_MAX_NAME_LEN] = {};
  if (zx_status_t result = vmo->get_name(vmo_name); result != ZX_OK) {
    return fit::error(ElfLoadError(ErrorType::GetVmoName, result));
  }

  auto visitor = [mapper_relative_bias, mapper, &vmo_name, &vmo, &diag](const auto& hdr) mutable {
    // Shift the start of the mapping down to the nearest page.
    auto adjust = util::page_offset(hdr.offset());
    auto file_offset = hdr.offset() - adjust;
    auto file_size = hdr.filesz() + static_cast<uint64_t>(adjust);
    auto virt_offset = hdr.vaddr() - static_cast<uint64_t>(adjust);
    auto virt_size = hdr.memsz() + static_cast<uint64_t>(adjust);

    printf("adjust:%lu\n", adjust);
    printf("file_offset:%lu\n", file_offset);
    printf("file_size:%lu\n", file_size);
    printf("virt_offset:%lu\n", virt_offset);
    printf("virt_size:%lu\n", virt_size);

    // Calculate the virtual address range that this mapping needs to cover.These addresses
    // are relative to the allocated VMAR, not the root VMAR.
    // TODO (Herrera): wrapping_add
    auto virt_addr = virt_offset + mapper_relative_bias;

    printf("virt_addr:0x%lx\n", virt_addr);

    // If the segment is specified as larger than the data in the file, and the data in the file
    // does not end at a page boundary, we will need to zero out the remaining memory in the
    // page.
    auto must_write =
        (virt_size > file_size) && (util::page_offset(static_cast<size_t>(file_size)) != 0);

    printf("must_write :%s\n", must_write ? "true" : "false");

    // If this segment is writeable (and we're mapping in some VMO content, i.e. it's not
    // all zero initialized) or the segment has a BSS section that needs to be zeroed, create
    // a writeable clone of the VMO. Otherwise use the potentially read-only VMO passed in.
    fbl::RefPtr<VmObjectDispatcher> vmo_to_map;
    if (must_write || ((file_size > 0) && hdr.writable())) {
      KernelHandle<VmObjectDispatcher> handle;
      fbl::RefPtr<VmObject> writeable_vmo;
      zx_rights_t rights;

      printf("hdr.writable() :%s\n", hdr.writable() ? "true" : "false");

      vmo->CreateChild(ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE, static_cast<uint64_t>(file_offset),
                       util::page_end(static_cast<uint64_t>(file_size)), false, &writeable_vmo);
      auto name = vmo_name_with_prefix<kVmoNamePrefixData>(vmo_name);
      if (zx_status_t result = writeable_vmo->set_name(name.data(), name.size()); result != ZX_OK) {
        diag.SystemError("ErrorType::SetVmoName(%d)", result);
        return false;
      }
      if (zx_status_t status = VmObjectDispatcher::Create(
              ktl::move(writeable_vmo), PAGE_SIZE * 2ul,
              VmObjectDispatcher::InitialMutability::kMutable, &handle, &rights);
          status != ZX_OK) {
        diag.SystemError("ElfLoadError::VmoCreate(%d)", status);
        return false;
      }

      // Update addresses into the VMO that will be mapped.
      file_offset = 0;

      // Zero-out the memory between the end of the filesize and the end of the page.
      if (virt_size > file_size) {
        // If the space to be zero-filled overlaps with the VMO, we need to memset it.
        auto memset_size =
            util::page_end(static_cast<size_t>(file_size)) - static_cast<size_t>(file_size);
        if (memset_size > 0) {
          if (zx_status_t result = handle.dispatcher()->vmo()->ZeroRange(
                  static_cast<size_t>(file_size), memset_size);
              result != ZX_OK) {
            diag.SystemError("ZeroRange failed");
            return false;
          }
        }
      }
      vmo_to_map = handle.release();
    } else {
      vmo_to_map = vmo;
    }

    // Create the VMO part of the mapping.
    // The VMO can be pager-backed, so include the ALLOW_FAULTS flag. ALLOW_FAULTS is a no-op
    // if not applicable to the VMO type.
    auto flags = ZX_VM_SPECIFIC | ZX_VM_ALLOW_FAULTS | ZX_VM_PERM_READ |
                 (hdr.writable() ? ZX_VM_PERM_WRITE : 0) |
                 (hdr.executable() ? ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED : 0);

    if (file_size != 0) {
      if (auto result = mapper->map(virt_addr, vmo_to_map, static_cast<uint64_t>(file_offset),
                                    util::page_end(static_cast<size_t>(file_size)), flags);
          result.is_error()) {
        diag.SystemError("ElfLoadError::VmarMap(%d)", result.error_value());
        return false;
      }
    }

    // If the mapping is specified as larger than the data in the file (i.e. virt_size is
    // larger than file_size), the remainder of the space (from virt_addr + file_size to
    // virt_addr + virt_size) is the BSS and must be filled with zeros.
    if (virt_size > file_size) {
      // The rest of the BSS is created as an anonymous vmo.
      auto bss_vmo_start = util::page_end(static_cast<size_t>(file_size));
      auto bss_vmo_size = util::page_end(static_cast<size_t>(virt_size)) - bss_vmo_start;
      if (bss_vmo_size > 0) {
        KernelHandle<VmObjectDispatcher> handle;
        zx_rights_t rights;
        zx::result<VmObjectDispatcher::CreateStats> parse_result =
            VmObjectDispatcher::parse_create_syscall_flags(0, bss_vmo_size);
        if (parse_result.is_error()) {
          // return fit::error(parse_result.error_value());
          return false;
        }
        VmObjectDispatcher::CreateStats stats = parse_result.value();

        fbl::RefPtr<VmObjectPaged> anon_vmo;
        if (zx_status_t status =
                VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, stats.flags, stats.size, &anon_vmo);
            status != ZX_OK) {
          diag.SystemError("ElfLoadError::VmoCreate(%d)", status);
          return false;
        }
        if (zx_status_t status = VmObjectDispatcher::Create(
                ktl::move(anon_vmo), PAGE_SIZE * 2ul,
                VmObjectDispatcher::InitialMutability::kMutable, &handle, &rights);
            status != ZX_OK) {
          diag.SystemError("ElfLoadError::VmoCreate(%d)", status);
          return false;
        }

        auto name = vmo_name_with_prefix<kVmoNamePrefixBss>(vmo_name);
        if (zx_status_t result = handle.dispatcher()->set_name(name.data(), name.size());
            result != ZX_OK) {
          diag.SystemError("ErrorType::SetVmoName(%d)", result);
          return false;
        }

        if (auto result =
                mapper->map(virt_addr + bss_vmo_start, handle.dispatcher(), 0, bss_vmo_size, flags);
            result.is_error()) {
          diag.SystemError("ElfLoadError::VmarMap(%d)", result.error_value());
          return false;
        }
      }
    }
    return true;
  };

  if (!load_info.VisitSegments(visitor)) {
    return fit::error(ElfLoadError(ErrorType::NothingToLoad, 0));
  }
  return fit::ok();
}

}  // namespace process_builder

// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/paging.h>

#include <ktl/string_view.h>
#include <phys/address-space.h>
#include <phys/elf-image.h>

#include "handoff-prep.h"

#include <ktl/enforce.h>

namespace {

constexpr arch::AccessPermissions ToAccessPermissions(PhysMapping::Permissions perms) {
  return {
      .readable = perms.readable(),
      .writable = perms.writable(),
      .executable = perms.executable(),
  };
}

}  // namespace

void HandoffPrep::CreateMapping(const PhysMapping& mapping, MappingType type) {
  AddressSpace::MapSettings settings;
  switch (type) {
    case MappingType::kNormal: {
      arch::AccessPermissions access = ToAccessPermissions(mapping.perms);
      settings = AddressSpace::NormalMapSettings(access);
    }
  }
  AddressSpace::PanicIfError(
      gAddressSpace->Map(mapping.vaddr, mapping.size, mapping.paddr, settings));
}

void HandoffPrep::PublishSingleMappingVmar(PhysMapping mapping, MappingType type) {
  PhysVmarPrep prep =
      PrepareVmarAt(ktl::string_view{mapping.name.data()}, mapping.vaddr, mapping.size);
  prep.PublishMapping(ktl::move(mapping), type);
  ktl::move(prep).Publish();
}

void HandoffPrep::ConstructKernelAddressSpace(const ElfImage& kernel) {
  // Physmap.
  {
    // Shadowing the entire physmap would be redundantly wasteful.
    PhysMapping mapping("physmap"sv, kArchPhysmapVirtualBase, kArchPhysmapSize, 0,
                        PhysMapping::Permissions::Rw(), /*kasan_shadow=*/false);
    PublishSingleMappingVmar(ktl::move(mapping), MappingType::kNormal);
  }

  // The kernel's mapping.
  {
    PhysVmarPrep prep = PrepareVmarAt("kernel"sv, kernel.load_address(), kernel.vaddr_size());
    kernel.load_info().VisitSegments([&kernel, &prep](const auto& segment) {
      uintptr_t vaddr = segment.vaddr() + kernel.load_bias();
      uintptr_t paddr = kernel.physical_load_address() + segment.offset();

      PhysMapping::Permissions perms = PhysMapping::Permissions{}
                                           .set_readable(segment.readable())
                                           .set_writable(segment.writable())
                                           .set_executable(segment.executable());
      // If the segment is executable and the hardware doesn't support
      // executable-only mappings, we fix up the permissions as also readable.
      if constexpr (!AddressSpace::kExecuteOnlyAllowed) {
        if (segment.executable()) {
          perms.set_readable();
        }
      }

      PhysMapping mapping({}, vaddr, segment.memsz(), paddr, perms);
      snprintf(mapping.name.data(), mapping.name.size(), "segment (p_vaddr = %#zx)",
               segment.vaddr());
      prep.PublishMapping(ktl::move(mapping), MappingType::kNormal);
      return true;
    });
    ktl::move(prep).Publish();
  }
}

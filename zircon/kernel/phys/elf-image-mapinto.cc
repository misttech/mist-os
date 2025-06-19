// Copyright 2022 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <phys/address-space.h>
#include <phys/elf-image.h>

// This method is in a separate file so that users of the phys:elf-image
// static_library() need not link in phys:address-space if they don't use it.

fit::result<AddressSpace::MapError> ElfImage::MapInto(AddressSpace& aspace) const {
  fit::result<AddressSpace::MapError> result = fit::ok();
  load_info().VisitSegments([&](const auto& segment) {
    uint64_t vaddr = segment.vaddr() + load_bias();
    uint64_t paddr = physical_load_address() + segment.offset();
    const AddressSpace::MapSettings settings = {
        .access =
            {
                .readable = segment.readable(),
                .writable = segment.writable(),
                .executable = segment.executable(),
            },
        .memory = kArchNormalMemoryType,
    };
    result = aspace.Map(vaddr, segment.memsz(), paddr, settings);
    return result.is_ok();
  });
  return result;
}

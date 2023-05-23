// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/debug_agent/elf_utils.h"

// clang-format off
// link.h contains ELF.h, which causes llvm/BinaryFormat/ELF.h fail to compile.
#include "src/lib/elflib/elflib.h"
// clang-format on

#include <link.h>

#include <set>
#include <string>

#include "src/developer/debug/debug_agent/process_handle.h"
#include "src/developer/debug/ipc/records.h"

namespace debug_agent {

namespace {

// Reads a null-terminated string from the given address of the given process.
debug::Status ReadNullTerminatedString(const ProcessHandle& process, zx_vaddr_t vaddr,
                                       std::string* dest) {
  // Max size of string we'll load as a sanity check.
  constexpr size_t kMaxString = 32768;

  dest->clear();

  constexpr size_t kBlockSize = 256;
  char block[kBlockSize];
  while (dest->size() < kMaxString) {
    size_t num_read = 0;
    if (auto status = process.ReadMemory(vaddr, block, kBlockSize, &num_read); status.has_error())
      return status;

    for (size_t i = 0; i < num_read; i++) {
      if (block[i] == 0)
        return debug::Status();
      dest->push_back(block[i]);
    }

    if (num_read < kBlockSize)
      return debug::Status();  // Partial read: hit the mapped memory boundary.
    vaddr += kBlockSize;
  }
  return debug::Status();
}

// Returns the fetch function for use by ElfLib for the given process. The ProcessHandle must
// outlive the returned value.
std::function<bool(uint64_t, std::vector<uint8_t>*)> GetElfLibReader(const ProcessHandle& process,
                                                                     uint64_t load_address) {
  return [&process, load_address](uint64_t offset, std::vector<uint8_t>* buf) {
    size_t num_read = 0;
    if (process.ReadMemory(load_address + offset, buf->data(), buf->size(), &num_read).has_error())
      return false;
    return num_read == buf->size();
  };
}

}  // namespace

debug::Status WalkElfModules(const ProcessHandle& process, uint64_t dl_debug_addr,
                             std::function<bool(uint64_t base_addr, uint64_t lmap)> cb) {
  size_t num_read = 0;
  uint64_t lmap = 0;
  if (auto status = process.ReadMemory(dl_debug_addr + offsetof(r_debug, r_map), &lmap,
                                       sizeof(lmap), &num_read);
      status.has_error())
    return status;

  size_t module_count = 0;

  // Walk the linked list.
  constexpr size_t kMaxObjects = 512;  // Sanity threshold.
  while (lmap != 0) {
    if (module_count++ >= kMaxObjects)
      return debug::Status("Too many modules, memory likely corrupted.");

    uint64_t base;
    if (process.ReadMemory(lmap + offsetof(link_map, l_addr), &base, sizeof(base), &num_read)
            .has_error())
      break;

    uint64_t next;
    if (process.ReadMemory(lmap + offsetof(link_map, l_next), &next, sizeof(next), &num_read)
            .has_error())
      break;

    if (!cb(base, lmap))
      break;

    lmap = next;
  }

  return debug::Status();
}

std::vector<debug_ipc::Module> GetElfModulesForProcess(const ProcessHandle& process,
                                                       uint64_t dl_debug_addr) {
  std::vector<debug_ipc::Module> modules;
  std::set<uint64_t> visited_modules;

  // Method 1: Use the dl_debug_addr, which should be the address of a |r_debug| struct.
  if (dl_debug_addr) {
    WalkElfModules(process, dl_debug_addr, [&](uint64_t base, uint64_t lmap) {
      debug_ipc::Module module;
      module.base = base;
      module.debug_address = lmap;

      uint64_t str_addr;
      size_t num_read;
      if (process
              .ReadMemory(lmap + offsetof(link_map, l_name), &str_addr, sizeof(str_addr), &num_read)
              .has_error())
        return false;

      if (ReadNullTerminatedString(process, str_addr, &module.name).has_error())
        return false;

      if (auto elf = elflib::ElfLib::Create(GetElfLibReader(process, module.base), module.base))
        module.build_id = elf->GetGNUBuildID();

      visited_modules.insert(module.base);
      modules.push_back(std::move(module));
      return true;
    });
  }

  // Method 2: Read the memory map and probe the ELF magic. This is secondary because it cannot
  // obtain the debug_address, which is used for resolving TLS location.
  std::vector<debug_ipc::AddressRegion> address_regions = process.GetAddressSpace(0);

  // With `-fuse-ld=lld -z noseparate-code`, multiple ELF segments could live on the same page and
  // get mapped multiple times with different flags. For example,
  //
  // Program Headers:
  //   Type           Offset   VirtAddr           PhysAddr           FileSiz  MemSiz   Flg Align
  //   LOAD           0x000000 0x0000000000000000 0x0000000000000000 0x000858 0x000858 R   0x1000
  //   LOAD           0x000860 0x0000000000001860 0x0000000000001860 0x000250 0x000250 R E 0x1000
  //   LOAD           0x000ab0 0x0000000000002ab0 0x0000000000002ab0 0x000220 0x000220 RW  0x1000
  //   LOAD           0x000cd0 0x0000000000003cd0 0x0000000000003cd0 0x000008 0x000008 RW  0x1000
  //
  // [zxdb] aspace
  //           Start              End  Prot   Size     Koid       Offset  Cmt.Pgs  Name
  //   0x15fb9584000    0x15fb9585000  r--      4K   479448          0x0        0  ...
  //   0x15fb9585000    0x15fb9586000  r-x      4K   479449          0x0        0  ...
  //   0x15fb9586000    0x15fb9587000  r--      4K   479450          0x0        0  ...
  //   0x15fb9587000    0x15fb9588000  rw-      4K   479451          0x0        0  ...
  //
  // and the debugger will see four ELF headers from 0x15fb9584000 to 0x15fb9587000. The third has
  // the same read-only protection at runtime because it contains read-only relocations.
  //
  // To solve this, we use a variable to track the end of the last module, and skip regions that
  // overlap with the last module. Other solutions include checking the VMO offset (assuming ELF
  // files always live from the beginning of VMOs), or checking whether build-id duplicates.
  uint64_t end_of_last_module = 0;
  for (const auto& region : address_regions) {
    if (region.base < end_of_last_module) {
      continue;
    }
    // With `-fuse-ld=ld -z noseparate-code`, ELF headers live together with the text section.
    if ((region.mmu_flags & ~ZX_VM_PERM_EXECUTE) != ZX_VM_PERM_READ) {
      continue;
    }
    if (!visited_modules.insert(region.base).second) {
      continue;
    }
    auto elf = elflib::ElfLib::Create(GetElfLibReader(process, region.base), region.base);
    if (!elf) {
      continue;
    }
    for (auto& phdr : elf->GetSegmentHeaders()) {
      if (phdr.p_type == PT_LOAD) {
        end_of_last_module = region.base + phdr.p_vaddr + phdr.p_memsz;
      }
    }

    std::string name = region.name;
    if (auto soname = elf->GetSoname()) {
      name = *soname;
    }
    modules.push_back(debug_ipc::Module{
        .name = std::move(name), .base = region.base, .build_id = elf->GetGNUBuildID()});
  }

  return modules;
}

}  // namespace debug_agent

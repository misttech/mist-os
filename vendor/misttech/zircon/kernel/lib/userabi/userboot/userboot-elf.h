// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_USERABI_USERBOOT_USERBOOT_ELF_H_
#define ZIRCON_KERNEL_LIB_USERABI_USERBOOT_USERBOOT_ELF_H_

#include <lib/elfldltl/layout.h>
#include <lib/zx/channel.h>
#include <lib/zx/debuglog.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <stddef.h>
#include <zircon/types.h>

#include <string_view>

class Bootfs;

struct LoadedElf {
  elfldltl::Elf<>::Ehdr header;
  zx::vmo vmo;
  zx_vaddr_t base = 0;
  zx_vaddr_t load_bias = 0;
};

struct ElfInfo {
  LoadedElf main_elf;
  LoadedElf interp_elf;
  bool has_interp;
};

// Returns the base address (p_vaddr bias).
zx_vaddr_t elf_load_vdso(const zx::debuglog& log, const zx::vmar& vmar, const zx::vmo& vmo);

// Returns the entry point address in the child, either to the named
// executable or to the PT_INTERP file loaded instead.  If the main
// file has a PT_INTERP, that name (with a fixed prefix applied) is
// also found in the bootfs and loaded instead of the main
// executable.  In that case, an extra zx_proc_args_t message is
// sent down the to_child pipe to prime the interpreter (presumably
// the dynamic linker) with the given log handle and a VMO for the
// main executable and a loader-service channel, the other end of
// which is returned here.
zx_vaddr_t elf_load_bootfs(const zx::debuglog& log, Bootfs& bootfs, std::string_view root,
                           const zx::process& proc, const zx::vmar& vmar, const zx::thread& thread,
                           std::string_view filename, size_t* stack_size, ElfInfo* info);

#endif  // ZIRCON_KERNEL_LIB_USERABI_USERBOOT_USERBOOT_ELF_H_

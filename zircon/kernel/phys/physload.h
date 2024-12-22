// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_PHYSLOAD_H_
#define ZIRCON_KERNEL_PHYS_PHYSLOAD_H_

// This is the API for a phys module loaded by physload.  A module is built for
// this using a kernel_elf_binary() GN target (see kernel_elf_binary.gni) that
// has //zircon/kernel/phys:physload.module in its deps list.  That target goes
// into the deps list of a kernel_package() target, and should be paired with a
// kernel_cmdline() target with `args = [ "kernel.phys.next=..." ]` matching
// the name of the binary's path in the kernel package, both in deps of some
// zbi() target to produce a bootable image.  (The cmdline is not needed for
// physboot itself, since it's the default for the boot option.)
//
// The physload ELF loader does simple-fixup relocation only, not symbolic
// dynamic linking.  The loaded module shares some code with physload, but only
// via function pointers such as the one inside the FILE object.  They share
// much more than that in data structures, but with separately-linked code to
// access them.  To ensure they always have matching code for each other's data
// structures, the `:physload.module` dependency sets a PT_INTERP string in the
// ELF module that is matched at runtime against physload's build ID: each
// module is built to go with a single physload binary that can load it.
//
// The module's ELF entry point must match the PhysLoadHandoffFunction type
// signature.  Linking it with `ldflags = [ "-Wl,-e,PhysLoadHandoff" ]` is the
// recommended way, so that this header file provides a declaration against
// which the definition can be checked at compile time.  The entry point
// function must not return.  It gets all the essential phys environment
// pointers for the state that physload has already set up.  The ElfImage for
// the module just entered can be found at `symbolize->modules().back()`.

#include <stdio.h>

#include <phys/handoff.h>
#include <phys/kernel-package.h>
#include <phys/uart.h>

class AddressSpace;
struct ArchPhysInfo;
struct BootOptions;
class ElfImage;
class Log;
class MainSymbolize;

namespace memalloc {
class Pool;
}  // namespace memalloc

using PhysLoadHandoffFunction =
    void(ElfImage& self,                   // The module just handed off to.
         Log* log,                         // Set gLog to this.
         ArchPhysInfo* arch_phys,          // Set gArchPhysInfo to this.
         UartDriver& uart,                 // From GetUartDriver().
         MainSymbolize* symbolize,         // Set gSymbolize to this.
         const BootOptions* boot_options,  // Set gBootOptions to this.
         memalloc::Pool& allocation_pool,  // Pass to Pool::InitWithPool.
         AddressSpace* aspace,             // Set gAddressSpace to this.
         PhysBootTimes boot_times,         // TODO(mcgrathr): more time points
         KernelStorage kernel_storage);

// The loaded module is linked with -W,-e,... to make this the entry symbol.
extern "C" [[noreturn]] PhysLoadHandoffFunction PhysLoadHandoff;

// PhysLoadHandoff calls PhysLoadModuleMain after initializing all the global
// variables that hold the rest of the state handed off.
extern "C" [[noreturn]] void PhysLoadModuleMain(UartDriver& uart, PhysBootTimes boot_times,
                                                KernelStorage kernel_storage);

// Arch-specific preparation in physload for heap and address space set-up (
// i.e., before calling InitMemory()).
void ArchPhysloadBeforeInitMemory();

// This should be run early in the handoff function to reinitialize
// arch-specific state that's not included in the handoff data.
// PhysLoadHandoff calls it before PhysLoadModuleMain.
void ArchOnPhysLoadHandoff();

#endif  // ZIRCON_KERNEL_PHYS_PHYSLOAD_H_

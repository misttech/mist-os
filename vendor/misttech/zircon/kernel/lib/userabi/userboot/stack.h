
// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_USERBOOT_STACK_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_USERBOOT_STACK_H_

#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <string_view>

#include <fbl/static_vector.h>

#include "util.h"

/*
 * Boot command-line arguments
 */
constexpr int kMaxInitArgs = CONFIG_INIT_ENV_ARG_LIMIT + 2;
constexpr int kMaxInitEnvs = CONFIG_INIT_ENV_ARG_LIMIT + 2;

#if __x86_64__
#define AT_VECTOR_SIZE_ARCH 2
#define AT_SYSINFO_EHDR 33
#endif

#define AT_VECTOR_SIZE_BASE 22 /* NEW_AUX_ENT entries in auxiliary table */

/* Symbolic values for the entries in the auxiliary table
   put on the initial stack */
#define AT_NULL 0      /* end of vector */
#define AT_IGNORE 1    /* entry should be ignored */
#define AT_EXECFD 2    /* file descriptor of program */
#define AT_PHDR 3      /* program headers for program */
#define AT_PHENT 4     /* size of program header entry */
#define AT_PHNUM 5     /* number of program headers */
#define AT_PAGESZ 6    /* system page size */
#define AT_BASE 7      /* base address of interpreter */
#define AT_FLAGS 8     /* flags */
#define AT_ENTRY 9     /* entry point of program */
#define AT_NOTELF 10   /* program is not ELF */
#define AT_UID 11      /* real uid */
#define AT_EUID 12     /* effective uid */
#define AT_GID 13      /* real gid */
#define AT_EGID 14     /* effective gid */
#define AT_PLATFORM 15 /* string identifying CPU for optimizations */
#define AT_HWCAP 16    /* arch dependent hints at CPU capabilities */
#define AT_CLKTCK 17   /* frequency at which times() increments */
/* AT_* values 18 through 22 are reserved */
#define AT_SECURE 23            /* secure mode boolean */
#define AT_BASE_PLATFORM 24     /* string identifying real platform, may differ from AT_PLATFORM. */
#define AT_RANDOM 25            /* address of 16 random bytes */
#define AT_HWCAP2 26            /* extension of AT_HWCAP */
#define AT_RSEQ_FEATURE_SIZE 27 /* rseq supported feature size */
#define AT_RSEQ_ALIGN 28        /* rseq allocation alignment */

#define AT_EXECFN 31 /* filename of program */

constexpr int kMaxAuvxSize = (2 * (AT_VECTOR_SIZE_ARCH + AT_VECTOR_SIZE_BASE + 1));

struct StackResult {
  zx_vaddr_t stack_pointer;
  zx_vaddr_t auxv_start;
  zx_vaddr_t auxv_end;
  zx_vaddr_t argv_start;
  zx_vaddr_t argv_end;
  zx_vaddr_t environ_start;
  zx_vaddr_t environ_end;
};

size_t get_initial_stack_size(
    const std::string_view& path, const fbl::static_vector<std::string_view, kMaxInitArgs>& argv,
    const fbl::static_vector<std::string_view, kMaxInitArgs>& environ,
    const fbl::static_vector<std::pair<uint32_t, uint64_t>, kMaxAuvxSize>& auxv);

StackResult populate_initial_stack(
    const zx::debuglog& log, zx::vmo& stack_vmo, const std::string_view& path,
    const fbl::static_vector<std::string_view, kMaxInitArgs>& argv,
    const fbl::static_vector<std::string_view, kMaxInitEnvs>& envp,
    fbl::static_vector<std::pair<uint32_t, uint64_t>, kMaxAuvxSize>& auxv, zx_vaddr_t stack_base,
    zx_vaddr_t original_stack_start_addr);

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_USERABI_USERBOOT_STACK_H_

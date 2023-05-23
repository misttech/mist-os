// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSCALLS_NEXT_H_
#define ZIRCON_SYSCALLS_NEXT_H_

#ifndef _KERNEL

#include <zircon/syscalls.h>

__BEGIN_CDECLS

// Make sure this matches <zircon/syscalls.h>.
#define _ZX_SYSCALL_DECL(name, type, attrs, nargs, arglist, prototype) \
  extern attrs type zx_##name prototype;                               \
  extern attrs type _zx_##name prototype;

#ifdef __clang__
#define _ZX_SYSCALL_ANNO(attr) __attribute__((attr))
#else
#define _ZX_SYSCALL_ANNO(attr)  // Nothing for compilers without the support.
#endif

#include <zircon/syscalls/internal/cdecls-next.inc>

#undef _ZX_SYSCALL_ANNO
#undef _ZX_SYSCALL_DECL

__END_CDECLS

#endif  // !_KERNEL

#include <stdint.h>
#include <zircon/compiler.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>

// ====== Pager writeback support ====== //
//
// Make sure the constants defined here do not overlap with VMO / pager constants defined in
// <zircon/types.h> or <zircon/syscalls/port.h>. These constants will eventually get moved over.

// VM Object creation options
#define ZX_VMO_TRAP_DIRTY ((uint32_t)1u << 3)

// Pager opcodes
#define ZX_PAGER_OP_DIRTY ((uint32_t)2u)
#define ZX_PAGER_OP_WRITEBACK_BEGIN ((uint32_t)3u)
#define ZX_PAGER_OP_WRITEBACK_END ((uint32_t)4u)

// zx_packet_page_request_t::command
#define ZX_PAGER_VMO_DIRTY ((uint16_t)2)

// Range type used by the zx_pager_query_dirty_ranges() syscall.
typedef struct zx_vmo_dirty_range {
  // Represents the range [offset, offset + length).
  uint64_t offset;
  uint64_t length;
  // Any options applicable to the range.
  // ZX_VMO_DIRTY_RANGE_IS_ZERO indicates that the range contains all zeros.
  uint64_t options;
} zx_vmo_dirty_range_t;

// options flags for zx_vmo_dirty_range_t
#define ZX_VMO_DIRTY_RANGE_IS_ZERO ((uint64_t)1u)

// Struct used by the zx_pager_query_vmo_stats() syscall.
typedef struct zx_pager_vmo_stats {
  // Will be set to ZX_PAGER_VMO_STATS_MODIFIED if the VMO was modified, or 0 otherwise.
  // Note that this can be set to 0 if a previous zx_pager_query_vmo_stats() call specified the
  // ZX_PAGER_RESET_VMO_STATS option, which resets the modified state.
  uint32_t modified;
} zx_pager_vmo_stats_t;

// values for zx_pager_vmo_stats.modified
#define ZX_PAGER_VMO_STATS_MODIFIED ((uint32_t)1u)

// options for zx_pager_query_vmo_stats()
#define ZX_PAGER_RESET_VMO_STATS ((uint32_t)1u)

// ====== End of pager writeback support ====== //

// ====== Restricted mode support ====== //
// Structures used for the experimental restricted mode syscalls.
// Declared here in the next syscall header since it is not published
// in the SDK.

#define ZX_RESTRICTED_OPT_EXCEPTION_CHANNEL ((uint32_t)1)

typedef uint64_t zx_restricted_reason_t;

// Reason codes provided to normal mode when a restricted process traps
// back to normal mode.
// clang-format off
#define ZX_RESTRICTED_REASON_SYSCALL   ((zx_restricted_reason_t)0)
#define ZX_RESTRICTED_REASON_EXCEPTION ((zx_restricted_reason_t)1)
#define ZX_RESTRICTED_REASON_KICK      ((zx_restricted_reason_t)2)
// clang-format on

// Structure to read and write restricted mode state
//
// When exiting restricted mode for certain reasons, additional information
// may be provided by zircon. However, regardless of the reason code this
// will always be the first structure inside the restricted mode state VMO.
#if __aarch64__
typedef struct zx_restricted_state {
  uint64_t x[31];
  uint64_t sp;
  uint64_t pc;
  uint64_t tpidr_el0;
  // Contains only the user-controllable upper 4-bits (NZCV).
  uint32_t cpsr;
  uint8_t padding1[4];
} zx_restricted_state_t;
#elif __x86_64__
typedef struct zx_restricted_state {
  // User space active registers
  uint64_t rdi, rsi, rbp, rbx, rdx, rcx, rax, rsp;
  uint64_t r8, r9, r10, r11, r12, r13, r14, r15;
  uint64_t ip, flags;

  uint64_t fs_base, gs_base;
} zx_restricted_state_t;
#elif __riscv
typedef zx_riscv64_thread_state_general_regs_t zx_restricted_state_t;
#else
#error what architecture?
#endif

// Structure populated by zircon when exiting restricted mode with the
// reason code `ZX_RESTRICTED_REASON_SYSCALL`.
typedef struct zx_restricted_syscall {
  // Must be first.
  zx_restricted_state_t state;
} zx_restricted_syscall_t;

// Structure populated by zircon when exiting restricted mode with the
// reason code `ZX_RESTRICTED_REASON_EXCEPTION`.
typedef struct zx_restricted_exception {
  // Must be first.
  zx_restricted_state_t state;
  zx_exception_report_t exception;
} zx_restricted_exception_t;

// ====== End of restricted mode support ====== //

// ====== Kernel-based memory attribution support ====== //
// Topic for zx_object_get_info.
#define ZX_INFO_MEMORY_ATTRIBUTION ((zx_object_info_topic_t)33u)  // zx_info_memory_attribution_t[n]

typedef struct zx_info_memory_attribution {
  // The koid of the process for which these attribution statistics apply.
  zx_koid_t process_koid;

  uint64_t private_resident_pages_allocated;
  uint64_t private_resident_pages_deallocated;

  uint64_t total_resident_pages_allocated;
  uint64_t total_resident_pages_deallocated;
} zx_info_memory_attribution_t;

// ====== End of kernel-based memory attribution support ====== //

#endif  // ZIRCON_SYSCALLS_NEXT_H_

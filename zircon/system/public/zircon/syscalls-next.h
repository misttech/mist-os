// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSCALLS_NEXT_H_
#define ZIRCON_SYSCALLS_NEXT_H_

#include <stdint.h>
#include <zircon/compiler.h>
#include <zircon/syscalls/debug.h>
#include <zircon/syscalls/exception.h>
#include <zircon/syscalls/iob.h>

// ====== Pager writeback support ====== //

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

// ====== End of pager writeback support ====== //

// ====== Restricted mode support ====== //
// Structures used for the experimental restricted mode syscalls.
// Declared here in the next syscall header since it is not published
// in the SDK.

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

// ====== Wake vector support ====== //

#define ZX_INTERRUPT_WAKE_VECTOR ((uint32_t)0x20)

// ====== End wake vector support ====== //

// ====== Software Sampling support ====== //

// The act of taking a sample takes on the order of single digit microseconds.
// A period close to or shorter than that doesn't make sense.
#define ZX_SAMPLER_MIN_PERIOD ZX_USEC(10)

#define ZX_SAMPLER_MAX_BUFFER_SIZE size_t(1024 * 1024 * 1024) /*1 GiB*/

// Configuration struct for periodically sampling a thread
typedef struct zx_sampler_config {
  zx_duration_mono_t period;
  size_t buffer_size;
  uint64_t iobuffer_discipline;
} zx_sampler_config_t;

// ====== End of Software Sampling support ====== //

// ====== Runtime processor power management support ====== //

typedef uint64_t zx_processor_power_level_options_t;

#define ZX_MAX_POWER_LEVELS ((uint64_t)(256))
#define ZX_MAX_POWER_LEVEL_TRANSFORMATIONS (ZX_MAX_POWER_LEVELS * ZX_MAX_POWER_LEVELS)

#define ZX_PROCESSOR_POWER_LEVEL_OPTIONS_DOMAIN_INDEPENDENT \
  ((zx_processor_power_level_options_t)(1u << 0))

// Specifies a processor power control interface.
typedef uint64_t zx_processor_power_control_t;

#define ZX_PROCESSOR_POWER_CONTROL_CPU_DRIVER ((zx_processor_power_control_t)(0u))
#define ZX_PROCESSOR_POWER_CONTROL_ARM_PSCI ((zx_processor_power_control_t)(1u))
#define ZX_PROCESSOR_POWER_CONTROL_ARM_WFI ((zx_processor_power_control_t)(2u))
#define ZX_PROCESSOR_POWER_CONTROL_RISCV_SBI ((zx_processor_power_control_t)(3u))
#define ZX_PROCESSOR_POWER_CONTROL_RISCV_WFI ((zx_processor_power_control_t)(4u))

// Extending port packets.
#define ZX_PKT_TYPE_PROCESSOR_POWER_LEVEL_TRANSITION_REQUEST ((zx_packet_type_t)0x0Au)

#define ZX_PKT_IS_PROCESSOR_POWER_LEVEL_TRANSITION_REQUEST(type) \
  ((type) == ZX_PKT_TYPE_PROCESSOR_POWER_LEVEL_TRANSITION_REQUEST)

// Represents a processor power level.
typedef struct {
  zx_processor_power_level_options_t options;
  uint64_t processing_rate;
  uint64_t power_coefficient_nw;
  zx_processor_power_control_t control_interface;
  uint64_t control_argument;
  char diagnostic_name[ZX_MAX_NAME_LEN];
  uint8_t reserved[32];
} zx_processor_power_level_t;

// Represents a transition between processor power levels.
typedef struct {
  // The expected latency of the transition.
  zx_duration_mono_t latency;

  // The expected energy expended by the transition in nanojoules.
  uint64_t energy_nj;

  // The level being transitioned from, represented as an index into an
  // associated array of zx_processor_power_level_t.
  uint8_t from;

  // The level being transitioned to, represented as an index into an
  // associated array of zx_processor_power_level_t.
  uint8_t to;

  uint8_t reserved[6];
} zx_processor_power_level_transition_t;

typedef struct {
  uint32_t domain_id;
  uint32_t options;
  zx_processor_power_control_t control_interface;
  uint64_t control_argument;
} zx_processor_power_state_t;

typedef struct {
  zx_cpu_set_t cpus;
  uint32_t domain_id;
  // Padding.
  uint8_t padding1[4];

} zx_processor_power_domain_t;

typedef struct {
  zx_cpu_set_t cpus;
  uint32_t domain_id;
  uint8_t idle_power_levels;
  uint8_t active_power_levels;
  uint8_t padding1[2];
} zx_power_domain_info_t;

// ====== End of runtime processor power management support ====== //
// ====== Upcoming IOB support ====== //

// Represents an IOBuffer region of "ID allocator" discipline, used to map sized
// data blobs to sequentially-allocated numeric IDs.
//
// Suppose there are N mapped blobs. The memory is laid out as follows, with
// copies of the blobs growing down and their corresponding bookkeeping indices
// growing up:
// --------------------------------
//   next available blob ID (4 bytes)
//   blob head offset (4 bytes)
//   ----------------------------
//   blob 0 size (4 bytes)       } <-- bookkeeping index
//   blob 0 offset (4 bytes)     }
//   ...
//   blob N-1 size (4 bytes)
//   blob N-1 offset (4 bytes)
//   ----------------------------
//   zero-initialized memory      <-- remaining bytes available
//   ---------------------------- <-- blob head offset
//   blob N-1
//   ...
//   blob 0
// --------------------------------
//
#define ZX_IOB_DISCIPLINE_TYPE_ID_ALLOCATOR ((zx_iob_discipline_type_t)(1u))

// Allocation options for `zx_iob_allocate_id()`.
typedef uint32_t zx_iob_allocate_id_options_t;

// ====== End of upcoming IOB support ====== //

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

#endif  // ZIRCON_SYSCALLS_NEXT_H_

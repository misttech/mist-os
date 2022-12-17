// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT. Generated from FIDL library zx by zither, a Fuchsia platform
// tool.

#ifndef _ZX_SYSCALL_DECL
#error "<zircon/syscalls-next.h> is the public API header"
#endif

_ZX_SYSCALL_DECL(pager_query_dirty_ranges, zx_status_t, /* no attributes */, 8,
                 (pager, pager_vmo, offset, length, buffer, buffer_size, actual, avail),
                 (_ZX_SYSCALL_ANNO(use_handle("Fuchsia")) zx_handle_t pager,
                  _ZX_SYSCALL_ANNO(use_handle("Fuchsia")) zx_handle_t pager_vmo, uint64_t offset,
                  uint64_t length, void* buffer, size_t buffer_size, size_t* actual, size_t* avail))

_ZX_SYSCALL_DECL(pager_query_vmo_stats, zx_status_t, /* no attributes */, 5,
                 (pager, pager_vmo, options, buffer, buffer_size),
                 (_ZX_SYSCALL_ANNO(use_handle("Fuchsia")) zx_handle_t pager,
                  _ZX_SYSCALL_ANNO(use_handle("Fuchsia")) zx_handle_t pager_vmo, uint32_t options,
                  void* buffer, size_t buffer_size))

_ZX_SYSCALL_DECL(process_create_shared, zx_status_t, /* no attributes */, 6,
                 (shared_proc, options, name, name_size, proc_handle, restricted_vmar_handle),
                 (_ZX_SYSCALL_ANNO(use_handle("Fuchsia")) zx_handle_t shared_proc, uint32_t options,
                  const char* name, size_t name_size,
                  _ZX_SYSCALL_ANNO(acquire_handle("Fuchsia")) zx_handle_t* proc_handle,
                  _ZX_SYSCALL_ANNO(acquire_handle("Fuchsia")) zx_handle_t* restricted_vmar_handle))

_ZX_SYSCALL_DECL(restricted_enter, zx_status_t, /* no attributes */, 3,
                 (options, vector_table_ptr, context),
                 (uint32_t options, uintptr_t vector_table_ptr, uintptr_t context))

_ZX_SYSCALL_DECL(restricted_read_state, zx_status_t, /* no attributes */, 2, (buffer, buffer_size),
                 (void* buffer, size_t buffer_size))

_ZX_SYSCALL_DECL(restricted_write_state, zx_status_t, /* no attributes */, 2, (buffer, buffer_size),
                 (const void* buffer, size_t buffer_size))

_ZX_SYSCALL_DECL(syscall_next_1, zx_status_t, /* no attributes */, 1, (arg), (int32_t arg))

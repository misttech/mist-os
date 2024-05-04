// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <inttypes.h>
#include <lib/mistos/zx_syscalls/util.h>
#include <lib/syscalls/forward.h>
#include <string.h>
#include <zircon/errors.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <object/channel_dispatcher.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/resource.h>
#include <object/resource_dispatcher.h>

#include <ktl/enforce.h>

// Create a new resource, child of the provided resource.
// On success, a new resource is created and handle is returned
// in |resource_out|.
//
// For more information on resources see docs/objects/resource.md
//
// The range low:high is inclusive on both ends, high must be
// greater than or equal low.
//
// |parent_rsrc| must be a resource of the same kind as |kind|, or
// ZX_RSRC_KIND_ROOT. |base| and |size| represent an inclusive range
// from |base| to |base| + |size| for the child resource.
zx_status_t zx_resource_create(zx_handle_t parent_rsrc, uint32_t options, uint64_t base,
                               size_t size, const char* user_name, size_t name_size,
                               zx_handle_t* resource_out) {
  ProcessDispatcher* up = nullptr;
  bool is_user_thread = (ThreadDispatcher::GetCurrent() != nullptr);
  if (is_user_thread) {
    up = ProcessDispatcher::GetCurrent();
  }

  // Verify that the given base and size don't lead to an integer overflow.
  if (base + size < base) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Obtain the parent Resource
  // WRITE access is required to create a child resource
  zx_status_t status;
  fbl::RefPtr<ResourceDispatcher> parent;
  status = handle_table(up).GetDispatcherWithRights(*up, parent_rsrc, ZX_RIGHT_WRITE, &parent);
  if (status) {
    return status;
  }

  uint32_t kind = ZX_RSRC_EXTRACT_KIND(options);
  uint32_t flags = ZX_RSRC_EXTRACT_FLAGS(options);
  if ((kind >= ZX_RSRC_KIND_COUNT) || (flags & ~ZX_RSRC_FLAGS_MASK)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Validate the parent resource the same way we would validate any
  // resource usage in another syscall.
  if (validate_ranged_resource(parent, kind, base, size) != ZX_OK) {
    return ZX_ERR_ACCESS_DENIED;
  }

  // If the resource is a slice of a larger resource then neither
  // the new resource nor its parent are permitted to be exclusive
  // resources. In this case, its |kind| will be something other
  // than ROOT and |parent_rsrc| will not be the ranged root resource for |kind|.
  if (parent->get_kind() != ZX_RSRC_KIND_ROOT && !parent->IsRangedRoot(kind) &&
      (parent->get_flags() & ZX_RSRC_FLAG_EXCLUSIVE || flags & ZX_RSRC_FLAG_EXCLUSIVE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Extract the name from userspace if one was provided.
  char name[ZX_MAX_NAME_LEN] = {0};
  size_t namesize = ktl::min(name_size, ZX_MAX_NAME_LEN - 1);
  if (name_size > 0) {
    memcpy(name, user_name, namesize);
  }

  // Create a new Resource
  zx_rights_t rights;
  KernelHandle<ResourceDispatcher> handle;
  status = ResourceDispatcher::Create(&handle, &rights, kind, base, size, flags, name);
  if (status != ZX_OK) {
    return status;
  }

  // Create a handle for the child
  return MakeAndAddHandle(ktl::move(handle), rights, resource_out);
}

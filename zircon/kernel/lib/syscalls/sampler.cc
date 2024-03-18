// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/boot-options/boot-options.h>
#include <lib/boot-options/types.h>
#include <lib/syscalls/forward.h>
#include <lib/thread_sampler/thread_sampler.h>

#include <object/io_buffer_dispatcher.h>
#include <object/resource.h>

#include "lib/user_copy/user_ptr.h"

#ifdef EXPERIMENTAL_THREAD_SAMPLER_ENABLED
constexpr bool kSamplerEnabled = EXPERIMENTAL_THREAD_SAMPLER_ENABLED;
#else
// The build system should always define the macro.
#error
#endif

// zx_status_t zx_sampler_create
zx_status_t sys_sampler_create(zx_handle_t rsrc, uint64_t options,
                               user_in_ptr<const zx_sampler_config_t> config,
                               zx_handle_t* buffers_out) {
  if constexpr (!kSamplerEnabled) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  *buffers_out = ZX_HANDLE_INVALID;
  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (zx_status_t status =
          validate_ranged_resource(rsrc, ZX_RSRC_KIND_SYSTEM, ZX_RSRC_SYSTEM_DEBUG_BASE, 1);
      status != ZX_OK) {
    return status;
  }

  // The sampler is a special case of an IOBuffer, so we use the same policy.
  auto up = ProcessDispatcher::GetCurrent();
  if (zx_status_t status = up->EnforceBasicPolicy(ZX_POL_NEW_IOB); status != ZX_OK) {
    return status;
  }

  zx_sampler_config_t sample_config;
  if (zx_status_t status = config.copy_from_user(&sample_config); status != ZX_OK) {
    return status;
  }

  // Validate the the provided config has reasonable values in it.
  //
  // Only the ZX_IOB_DISCIPLINE_TYPE_NONE is currently supported
  if (sample_config.iobuffer_discipline != ZX_IOB_DISCIPLINE_TYPE_NONE) {
    return ZX_ERR_INVALID_ARGS;
  }

  // We'll pick a arbitrary unreasonably large max size for the per cpu buffers.
  //
  // When we implement IOBuffer shared reading and writing, we can reduce this to something more
  // reasonable.
  if (sample_config.buffer_size > ZX_SAMPLER_MAX_BUFFER_SIZE) {
    return ZX_ERR_INVALID_ARGS;
  }

  // The act of taking a sample takes on the order of single digit microseconds. A period close to
  // or shorter than that doesn't make sense.
  if (sample_config.period < ZX_SAMPLER_MIN_PERIOD) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx::result<KernelHandle<sampler::ThreadSamplerDispatcher>> create_res =
      sampler::ThreadSamplerDispatcher::Create(sample_config);
  if (create_res.is_error()) {
    return create_res.status_value();
  }
  return up->MakeAndAddHandle(
      ktl::move(*create_res),
      (IoBufferDispatcher::default_rights() | ZX_RIGHT_APPLY_PROFILE) & ~ZX_RIGHT_WRITE,
      buffers_out);
}

// zx_status_t zx_sampler_attach
zx_status_t sys_sampler_attach(zx_handle_t iobuffer, zx_handle_t thread) {
  if constexpr (!kSamplerEnabled) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  auto up = ProcessDispatcher::GetCurrent();
  fbl::RefPtr<ThreadDispatcher> thread_dispatcher;

  if (zx_status_t status = up->handle_table().GetDispatcherWithRights(*up, thread, ZX_RIGHT_READ,
                                                                      &thread_dispatcher);
      status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<IoBufferDispatcher> thread_sampler;
  if (zx_status_t status = up->handle_table().GetDispatcherWithRights(
          *up, iobuffer, ZX_RIGHT_APPLY_PROFILE, &thread_sampler);
      status != ZX_OK) {
    return status;
  }

  return sampler::ThreadSamplerDispatcher::AddThread(thread_sampler, thread_dispatcher)
      .status_value();
}

// zx_status_t zx_sampler_start
zx_status_t sys_sampler_start(zx_handle_t iobuffer) {
  if constexpr (!kSamplerEnabled) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::RefPtr<IoBufferDispatcher> thread_sampler;
  auto up = ProcessDispatcher::GetCurrent();
  if (zx_status_t status = up->handle_table().GetDispatcherWithRights(
          *up, iobuffer, ZX_RIGHT_APPLY_PROFILE, &thread_sampler);
      status != ZX_OK) {
    return status;
  }

  return sampler::ThreadSamplerDispatcher::Start(thread_sampler).status_value();
}

// zx_status_t zx_sampler_stop
zx_status_t sys_sampler_stop(zx_handle_t iobuffer) {
  if constexpr (!kSamplerEnabled) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  if (!gBootOptions->enable_debugging_syscalls) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  fbl::RefPtr<IoBufferDispatcher> thread_sampler;
  auto up = ProcessDispatcher::GetCurrent();
  if (zx_status_t status = up->handle_table().GetDispatcherWithRights(
          *up, iobuffer, ZX_RIGHT_APPLY_PROFILE, &thread_sampler);
      status != ZX_OK) {
    return status;
  }

  return sampler::ThreadSamplerDispatcher::Stop(thread_sampler).status_value();
}

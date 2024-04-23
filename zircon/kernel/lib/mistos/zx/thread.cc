// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/zx/process.h>
#include <lib/mistos/zx/thread.h>

#include <object/process_dispatcher.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

zx_status_t thread::create(const process& process, const char* name, uint32_t name_len,
                           uint32_t flags, thread* result) {
  // Assume |result| and |process| must refer to different containers, due
  // to strict aliasing.

  if (!process.is_valid()) {
    return ZX_ERR_BAD_HANDLE;
  }

  // currently, the only valid option value is 0
  if (flags != 0)
    return ZX_ERR_INVALID_ARGS;

  // copy out the name
  char buf[ZX_MAX_NAME_LEN];
  ktl::string_view sp;

  // Silently truncate the given name.
  if (name_len > ZX_MAX_NAME_LEN)
    name_len = ZX_MAX_NAME_LEN;
  memcpy(buf, name, name_len);

  // ensure zero termination
  size_t str_len = (name_len == ZX_MAX_NAME_LEN ? name_len - 1 : name_len);
  buf[str_len] = 0;
  sp = ktl::string_view(buf);

  KernelHandle<ThreadDispatcher> handle;
  zx_rights_t thread_rights;
  zx_status_t status = ThreadDispatcher::Create(process.get(), flags, sp, &handle, &thread_rights);
  if (status != ZX_OK)
    return status;

  status = handle.dispatcher()->Initialize();
  if (status != ZX_OK) {
    return status;
  }

  result->reset(handle.release());
  return ZX_OK;
}

}  // namespace zx

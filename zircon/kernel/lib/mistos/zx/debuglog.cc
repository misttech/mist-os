// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/debuglog.h"

#include <lib/debuglog.h>
#include <trace.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

zx_status_t debuglog::create(const resource& resource, uint32_t options, debuglog* result) {
  // Ensure only valid options were given provided. The only valid flag is currently
  // ZX_LOG_FLAGS_READABLE.
  if ((options & ZX_LOG_FLAG_READABLE) != options) {
    return ZX_ERR_INVALID_ARGS;
  }

  debuglog d;
  // HACK: Set it self address to make is_valid() and ZX_ASSERT() to pass.
  d.reset(&d);
  result->reset(d.release());
  return ZX_OK;
}

zx_status_t debuglog::write(uint32_t options, const void* buffer, size_t buffer_size) const {
  LTRACEF("opt %x, ptr 0x%p, len %zu\n", options, buffer, buffer_size);

  buffer_size = buffer_size > DLOG_MAX_DATA ? DLOG_MAX_DATA : buffer_size;

  if (options & (~ZX_LOG_FLAGS_MASK))
    return ZX_ERR_INVALID_ARGS;

  return dlog_write(DEBUGLOG_INFO, options, {(char*)buffer, buffer_size});
}

zx_status_t debuglog::read(uint32_t options, void* buffer, size_t buffer_size) const {
  LTRACEF("opt %x, ptr 0x%p, len %zu\n", options, buffer, buffer_size);

  /*if (options != 0)
    return ZX_ERR_INVALID_ARGS;

  dlog_record_t record{};
  size_t actual;

  if (!(flags_ & ZX_LOG_FLAG_READABLE))
    return ZX_ERR_BAD_STATE;

  Guard<CriticalMutex> guard{get_lock()};

  zx_status_t status = reader_.Read(0, record, actual);
  if (status == ZX_ERR_SHOULD_WAIT) {
    UpdateStateLocked(ZX_CHANNEL_READABLE, 0);
  }

  return status;
  // if ((status = log->Read(options, &record, &actual)) < 0) {
  //   return status;
  // }*/
  return ZX_ERR_NOT_SUPPORTED;
}

}  // namespace zx

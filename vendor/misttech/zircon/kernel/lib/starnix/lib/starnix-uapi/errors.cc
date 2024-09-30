// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/errors.h>

#include <linux/errno.h>

// There isn't really a mapping from zx::Status to Errno. The correct mapping is context-speific
// but this converter is a reasonable first-approximation. The translation matches
// fdio_status_to_errno. See https://fxbug.dev/42105838 for more context.
// TODO: Replace clients with more context-specific mappings.
uint32_t from_status_like_fdio(zx_status_t status) {
  switch (status) {
    case ZX_ERR_NOT_FOUND:
      return ENOENT;
    case ZX_ERR_NO_MEMORY:
      return ENOMEM;
    case ZX_ERR_INVALID_ARGS:
      return EINVAL;
    case ZX_ERR_BUFFER_TOO_SMALL:
      return EINVAL;
    case ZX_ERR_TIMED_OUT:
      return ETIMEDOUT;
    case ZX_ERR_UNAVAILABLE:
      return EBUSY;
    case ZX_ERR_ALREADY_EXISTS:
      return EEXIST;
    case ZX_ERR_PEER_CLOSED:
      return EPIPE;
    case ZX_ERR_BAD_STATE:
      return EPIPE;
    case ZX_ERR_BAD_PATH:
      return ENAMETOOLONG;
    case ZX_ERR_IO:
      return EIO;
    case ZX_ERR_NOT_FILE:
      return EISDIR;
    case ZX_ERR_NOT_DIR:
      return ENOTDIR;
    case ZX_ERR_NOT_SUPPORTED:
      return EOPNOTSUPP;
    case ZX_ERR_WRONG_TYPE:
      return EOPNOTSUPP;
    case ZX_ERR_OUT_OF_RANGE:
      return EINVAL;
    case ZX_ERR_NO_RESOURCES:
      return ENOMEM;
    case ZX_ERR_BAD_HANDLE:
      return EBADF;
    case ZX_ERR_ACCESS_DENIED:
      return EACCES;
    case ZX_ERR_SHOULD_WAIT:
      return EAGAIN;
    case ZX_ERR_FILE_BIG:
      return EFBIG;
    case ZX_ERR_NO_SPACE:
      return ENOSPC;
    case ZX_ERR_NOT_EMPTY:
      return ENOTEMPTY;
    case ZX_ERR_IO_REFUSED:
      return ECONNREFUSED;
    case ZX_ERR_IO_INVALID:
      return EIO;
    case ZX_ERR_CANCELED:
      return EBADF;
    case ZX_ERR_PROTOCOL_NOT_SUPPORTED:
      return EPROTONOSUPPORT;
    case ZX_ERR_ADDRESS_UNREACHABLE:
      return ENETUNREACH;
    case ZX_ERR_ADDRESS_IN_USE:
      return EADDRINUSE;
    case ZX_ERR_NOT_CONNECTED:
      return ENOTCONN;
    case ZX_ERR_CONNECTION_REFUSED:
      return ECONNREFUSED;
    case ZX_ERR_CONNECTION_RESET:
      return ECONNRESET;
    case ZX_ERR_CONNECTION_ABORTED:
      return ECONNABORTED;
    default:
      return EIO;
  }
}

/// Maps `Err(EINTR)` to the specified errno.
fit::result<Errno> map_eintr(fit::result<Errno> result, Errno err) {
  if (result.is_error()) {
    if (result.error_value().error_code() == EINTR) {
      return fit::error(err);
    }
    return result.take_error();
  }
  return result;
}

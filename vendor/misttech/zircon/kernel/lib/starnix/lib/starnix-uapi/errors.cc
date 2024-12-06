// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/util/bstring.h>

#include <linux/errno.h>

namespace starnix_uapi {

BString to_string(const std::source_location& location) {
  return mtl::format("%s:%d:%d", location.file_name(), location.line(), location.column());
}

BString ErrnoCode::to_string() const {
  return mtl::format("%s(%d)", name_ ? name_ : "<null>", code_);
}

BString Errno::to_string() const {
  auto location = starnix_uapi::to_string(location_);
  auto code = code_.to_string();
  if (context_.has_value()) {
    return mtl::format("errno %.*s from %.*s, context: %.*s", static_cast<int>(code.size()),
                       code.data(), static_cast<int>(location.size()), location.data(),
                       static_cast<int>(context_->size()), context_->data());
  }
  return mtl::format("errno %.*s from %.*s", static_cast<int>(code.size()), code.data(),
                     static_cast<int>(location.size()), location.data());
}

uint32_t from_status_like_fdio(zx_status_t status) {
  switch (status) {
    case ZX_ERR_NOT_FOUND:
      return ENOENT;
    case ZX_ERR_NO_MEMORY:
      return ENOMEM;
    case ZX_ERR_INVALID_ARGS:
    case ZX_ERR_BUFFER_TOO_SMALL:
      return EINVAL;
    case ZX_ERR_TIMED_OUT:
      return ETIMEDOUT;
    case ZX_ERR_UNAVAILABLE:
      return EBUSY;
    case ZX_ERR_ALREADY_EXISTS:
      return EEXIST;
    case ZX_ERR_PEER_CLOSED:
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
fit::result<Errno> map_eintr(fit::result<Errno> result, const Errno& err) {
  if (result.is_error()) {
    if (result.error_value().error_code() == EINTR) {
      return fit::error(err);
    }
    return result.take_error();
  }
  return result;
}

}  // namespace starnix_uapi

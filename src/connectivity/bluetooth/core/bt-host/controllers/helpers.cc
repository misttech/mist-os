// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "helpers.h"

namespace bt::controllers {

pw::Status ZxStatusToPwStatus(zx_status_t status) {
  switch (status) {
    case ZX_OK:
      return PW_STATUS_OK;
    case ZX_ERR_INTERNAL:
      return pw::Status::Internal();
    case ZX_ERR_NOT_SUPPORTED:
      return pw::Status::Unimplemented();
    case ZX_ERR_NO_RESOURCES:
      return pw::Status::ResourceExhausted();
    case ZX_ERR_INVALID_ARGS:
      return pw::Status::InvalidArgument();
    case ZX_ERR_OUT_OF_RANGE:
      return pw::Status::OutOfRange();
    case ZX_ERR_CANCELED:
      return pw::Status::Cancelled();
    case ZX_ERR_PEER_CLOSED:
      return pw::Status::Unavailable();
    case ZX_ERR_NOT_FOUND:
      return pw::Status::NotFound();
    case ZX_ERR_ALREADY_EXISTS:
      return pw::Status::AlreadyExists();
    case ZX_ERR_UNAVAILABLE:
      return pw::Status::Unavailable();
    case ZX_ERR_ACCESS_DENIED:
      return pw::Status::PermissionDenied();
    case ZX_ERR_IO_DATA_LOSS:
      return pw::Status::DataLoss();
    default:
      return pw::Status::Unknown();
  }
}

}  // namespace bt::controllers

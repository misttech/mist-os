// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_CHANNEL_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_CHANNEL_H_

#include <Python.h>

#include "fidl_handle.h"
#include "macros.h"

namespace fidl_channel {

extern PyTypeObject FidlChannelType;

IGNORE_EXTRA_SC
using FidlChannel = struct {
  fidl_handle::FidlHandle super;
};

}  // namespace fidl_channel

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_ABI_FIDL_CHANNEL_H_

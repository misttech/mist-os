// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_ERROR_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_ERROR_H_
#include <zircon/errors.h>
#include <zircon/types.h>

#include "macros.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/python/py_header.h"
namespace error {

// The (singleton) type of the ZxStatus error type. Must be initialized using ZxStatusType_Create().
extern PyTypeObject* ZxStatusType;

// Creates the ZxStatus error type. Must only be run once.
extern PyTypeObject* ZxStatusType_Create();

}  // namespace error
#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_CPP_ABI_ERROR_H_

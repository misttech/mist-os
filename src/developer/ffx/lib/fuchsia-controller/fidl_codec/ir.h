// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_IR_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_IR_H_

#include <Python.h>

#include "src/lib/fidl_codec/library_loader.h"

namespace ir {

PyObject *add_ir_path(PyObject *self, PyObject *path_obj);
extern PyMethodDef add_ir_path_py_def;

}  // namespace ir

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_IR_H_

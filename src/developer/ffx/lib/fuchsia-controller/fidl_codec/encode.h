// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_ENCODE_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_ENCODE_H_

#include <Python.h>

namespace encode {

PyObject *encode_fidl_message(PyObject *self, PyObject *args, PyObject *kwds);
extern PyMethodDef encode_fidl_message_py_def;

}  // namespace encode

#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_ENCODE_H_

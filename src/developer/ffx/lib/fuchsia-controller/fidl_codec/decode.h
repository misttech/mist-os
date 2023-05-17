// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_DECODE_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_DECODE_H_
#include <Python.h>

namespace decode {

// Attempts to decode a FIDL response message. The expected arguments are "bytes" which should be a
// bytearray object representing the message to be decoded, and "handles" which should be a list of
// integers that are able to be converted into an unsigned 32 bit integer.
PyObject *decode_fidl_response(PyObject *self, PyObject *args, PyObject *kwds);

// Attempts to decode a FIDL request message. The expected arguments are "bytes" which should be a
// bytearray object representing the message to be decoded, and "handles" which should be a list of
// integers that are able to be converted into an unsigned 32 bit integer.
PyObject *decode_fidl_request(PyObject *self, PyObject *args, PyObject *kwds);

// Represents the signature of the FIDL response decoder method. Used to register the function in a
// Python module.
extern PyMethodDef decode_fidl_response_py_def;

// Represents the signature of the FIDL request decoder method. Used to register the function in a
// Python module.
extern PyMethodDef decode_fidl_request_py_def;

}  // namespace decode
#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_DECODE_H_

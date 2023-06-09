// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "decode.h"

#include <cinttypes>
#include <iostream>
#include <vector>

#include "mod.h"
#include "python_dict_visitor.h"
#include "src/developer/ffx/lib/fuchsia-controller/abi/convert.h"
#include "src/lib/fidl_codec/wire_parser.h"

namespace decode {

enum Direction { REQUEST, RESPONSE };

PyObject *decode_fidl_message(PyObject *self, PyObject *args, PyObject *kwds,  // NOLINT
                              Direction direction) {
  PyObject *bytes = nullptr;
  PyObject *handles = nullptr;
  static const char *kwlist[] = {
      "bytes",
      "handles",
      nullptr,
  };
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "OO", const_cast<char **>(kwlist), &bytes,
                                   &handles)) {
    return nullptr;
  }
  // NOLINTNEXTLINE: This macro upsets the linter for not using DeMorgan's.
  if (!PyByteArray_Check(bytes)) {
    PyErr_SetString(PyExc_TypeError, "Expected bytes to be bytearray");
    return nullptr;
  }
  if (!PyList_Check(handles)) {
    PyErr_SetString(PyExc_TypeError, "Expected handles to be a list");
    return nullptr;
  }
  auto display_options = DisplayOptions{
      .pretty_print = true,
      .with_process_info = false,
      .needs_colors = false,
  };
  char *c_bytes = PyByteArray_AsString(bytes);
  if (c_bytes == nullptr) {
    return nullptr;
  }
  Py_ssize_t c_bytes_len = PyByteArray_GET_SIZE(bytes);
  Py_ssize_t c_handles_len = PyList_Size(handles);
  std::unique_ptr<zx_handle_disposition_t[]> c_handles =
      std::make_unique<zx_handle_disposition_t[]>(c_handles_len);
  for (Py_ssize_t i = 0; i < c_handles_len; ++i) {
    auto obj = PyList_GetItem(handles, i);
    if (obj == nullptr) {
      return nullptr;
    }
    zx_handle_t res = convert::PyLong_AsU32(obj);
    if (res == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
      return nullptr;
    }
    // TODO(fxbug.dev/124288): Properly fill the handle disposition. The code will likely
    // not just be integers.
    c_handles[i] = zx_handle_disposition_t{.handle = res};
  }
  auto header = reinterpret_cast<const fidl_message_header_t *>(c_bytes);
  const std::vector<const fidl_codec::ProtocolMethod *> *methods =
      mod::get_module_state()->loader->GetByOrdinal(header->ordinal);
  if (methods == nullptr || methods->empty()) {
    PyErr_Format(PyExc_LookupError, "Unable to find any methods for method ordinal: %" PRIu64,
                 header->ordinal);
    return nullptr;
  }
  // What is the approach here if there's more than one method?
  const fidl_codec::ProtocolMethod *method = (*methods)[0];
  std::unique_ptr<fidl_codec::PayloadableValue> object;
  std::ostringstream errors;
  bool successful;
  switch (direction) {
    case Direction::RESPONSE:
      successful = fidl_codec::DecodeResponse(
          method, reinterpret_cast<uint8_t *>(c_bytes), static_cast<uint64_t>(c_bytes_len),
          c_handles.get(), static_cast<uint64_t>(c_handles_len), &object, errors);
      break;
    case Direction::REQUEST:
      successful = fidl_codec::DecodeRequest(method, reinterpret_cast<uint8_t *>(c_bytes),
                                             static_cast<uint64_t>(c_bytes_len), c_handles.get(),
                                             static_cast<uint64_t>(c_handles_len), &object, errors);
      break;
  }
  if (!successful) {
    PyErr_SetString(PyExc_IOError, errors.str().c_str());
    return nullptr;
  }
  if (object == nullptr) {
    PyErr_SetString(PyExc_RuntimeError, "Parsed object is null");
    return nullptr;
  }
  python_dict_visitor::PythonDictVisitor visitor;
  object->Visit(&visitor, nullptr);
  return visitor.result();
}

PyObject *decode_fidl_response(PyObject *self, PyObject *args, PyObject *kwds) {  // NOLINT
  return decode_fidl_message(self, args, kwds, Direction::RESPONSE);
}

PyObject *decode_fidl_request(PyObject *self, PyObject *args, PyObject *kwds) {  // NOLINT
  return decode_fidl_message(self, args, kwds, Direction::REQUEST);
}

PyMethodDef decode_fidl_response_py_def = {
    "decode_fidl_response", reinterpret_cast<PyCFunction>(decode_fidl_response),
    METH_VARARGS | METH_KEYWORDS, "Decodes a FIDL response message from bytes and handles."};

PyMethodDef decode_fidl_request_py_def = {
    "decode_fidl_request", reinterpret_cast<PyCFunction>(decode_fidl_request),
    METH_VARARGS | METH_KEYWORDS, "Decodes a FIDL request message from bytes and handles."};

}  // namespace decode

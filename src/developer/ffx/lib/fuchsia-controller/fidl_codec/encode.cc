// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "encode.h"

#include <iostream>
#include <string>

#include "mod.h"
#include "object_converter.h"
#include "py_wrapper.h"
#include "src/developer/ffx/lib/fuchsia-controller/abi/convert.h"
#include "src/lib/fidl_codec/encoder.h"
#include "src/lib/fidl_codec/wire_types.h"

namespace encode {

PyMethodDef encode_fidl_message_py_def = {
    "encode_fidl_message", reinterpret_cast<PyCFunction>(encode_fidl_message),
    METH_VARARGS | METH_KEYWORDS,
    "Encodes the FIDL wire format representation of the object. "
    "The only necessary fields are txid and ordinal. Everything else can be set to None. "
    "If the object field is not None, then all parameters are required. "
    "If object is None, other optional parameters will be ignored."};

struct GetPayloadTypeArgs {
  PyObject *obj;
  PyObject *type_name_obj;
  PyObject *library_obj;
};

std::unique_ptr<fidl_codec::Type> GetPayloadType(GetPayloadTypeArgs args) {
  if (args.obj == Py_None) {
    return std::make_unique<fidl_codec::EmptyPayloadType>();
  }
  const char *lib_c_str = PyUnicode_AsUTF8(args.library_obj);
  if (lib_c_str == nullptr) {
    return nullptr;
  }
  const char *type_name_c_str = PyUnicode_AsUTF8(args.type_name_obj);
  if (type_name_c_str == nullptr) {
    return nullptr;
  }
  std::string lib_str(lib_c_str);
  std::string type_name_str(type_name_c_str);
  auto library = mod::get_ir_library(lib_str);
  if (library == nullptr) {
    return nullptr;
  }
  auto type = library->TypeFromIdentifier(false, type_name_str);
  if (type == nullptr) {
    PyErr_Format(PyExc_RuntimeError, "Unrecognized type: '%s'", type_name_c_str);
    return nullptr;
  }
  return type;
}

// NOLINTNEXTLINE: similarly typed parameters are unavoidable in Python.
PyObject *encode_fidl_message(PyObject *self, PyObject *args, PyObject *kwds) {
  static constexpr uint8_t HEADER_MAGIC = 1;
  static constexpr uint8_t AT_REST_FLAGS[2] = {FIDL_MESSAGE_HEADER_AT_REST_FLAGS_0_USE_VERSION_V2,
                                               0};
  static constexpr uint8_t DYNAMIC_FLAGS = 0;

  static const char *kwlist[] = {"object", "library", "type_name", "txid", "ordinal", nullptr};
  PyObject *obj = nullptr;
  PyObject *library_obj = nullptr;
  PyObject *type_name_obj = nullptr;
  PyObject *ordinal_obj = nullptr;
  PyObject *txid_obj = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "OOOOO", const_cast<char **>(kwlist), &obj,
                                   &library_obj, &type_name_obj, &txid_obj, &ordinal_obj)) {
    return nullptr;
  }

  if (ordinal_obj == Py_None || txid_obj == Py_None) {
    PyErr_SetString(PyExc_TypeError, "ordinal and txid must not be None");
    return nullptr;
  }

  auto ordinal = convert::PyLong_AsU64(ordinal_obj);
  if (ordinal == convert::MINUS_ONE_U64 && PyErr_Occurred()) {
    return nullptr;
  }

  auto txid = convert::PyLong_AsU32(txid_obj);
  if (txid == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
    return nullptr;
  }

  auto type = GetPayloadType(GetPayloadTypeArgs{
      .obj = obj,
      .type_name_obj = type_name_obj,
      .library_obj = library_obj,
  });
  if (type == nullptr) {
    return nullptr;
  }
  auto converted = converter::ObjectConverter::Convert(obj, type.get());
  if (converted == nullptr) {
    return nullptr;
  }
  auto msg = fidl_codec::Encoder::EncodeMessage(txid, ordinal, AT_REST_FLAGS, DYNAMIC_FLAGS,
                                                HEADER_MAGIC, converted.get(), type.get());
  auto res = py::Object(PyTuple_New(2));
  if (res == nullptr) {
    return nullptr;
  }
  auto buf = py::Object(PyByteArray_FromStringAndSize(
      reinterpret_cast<const char *>(msg.bytes.data()), static_cast<Py_ssize_t>(msg.bytes.size())));
  if (buf == nullptr) {
    return nullptr;
  }
  auto handles_list = py::Object(PyList_New(static_cast<Py_ssize_t>(msg.handles.size())));
  if (handles_list == nullptr) {
    return nullptr;
  }
  PyTuple_SET_ITEM(res.get(), 0, buf.take());
  for (uint64_t i = 0; i < msg.handles.size(); ++i) {
    // TODO(fxbug.dev/124288): Just assumes when encoding that everything here is an integer.
    auto handle_obj = py::Object(PyLong_FromLong(msg.handles[i].handle));
    if (handle_obj == nullptr) {
      return nullptr;
    }
    PyList_SET_ITEM(handles_list.get(), static_cast<Py_ssize_t>(i), handle_obj.take());
  }
  PyTuple_SET_ITEM(res.get(), 1, handles_list.take());
  return res.take();
}

}  // namespace encode

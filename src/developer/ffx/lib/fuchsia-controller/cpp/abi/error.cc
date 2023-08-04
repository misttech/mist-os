// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#include "error.h"

#include <structmember.h>  // PyMemberDef.

#include <iostream>
#include <sstream>

namespace error {

// TODO(fxbug.dev/127163): This has been copied from zircon code, as vdso doesn't build this for
// host.
const char *zx_status_get_string(zx_status_t status) {
  switch (status) {
    case ZX_OK:
      return "ZX_OK";
    case ZX_ERR_INTERNAL:
      return "ZX_ERR_INTERNAL";
    case ZX_ERR_NOT_SUPPORTED:
      return "ZX_ERR_NOT_SUPPORTED";
    case ZX_ERR_NO_RESOURCES:
      return "ZX_ERR_NO_RESOURCES";
    case ZX_ERR_NO_MEMORY:
      return "ZX_ERR_NO_MEMORY";
    case ZX_ERR_INTERNAL_INTR_RETRY:
      return "ZX_ERR_INTERNAL_INTR_RETRY";
    case ZX_ERR_INVALID_ARGS:
      return "ZX_ERR_INVALID_ARGS";
    case ZX_ERR_BAD_HANDLE:
      return "ZX_ERR_BAD_HANDLE";
    case ZX_ERR_WRONG_TYPE:
      return "ZX_ERR_WRONG_TYPE";
    case ZX_ERR_BAD_SYSCALL:
      return "ZX_ERR_BAD_SYSCALL";
    case ZX_ERR_OUT_OF_RANGE:
      return "ZX_ERR_OUT_OF_RANGE";
    case ZX_ERR_BUFFER_TOO_SMALL:
      return "ZX_ERR_BUFFER_TOO_SMALL";
    case ZX_ERR_BAD_STATE:
      return "ZX_ERR_BAD_STATE";
    case ZX_ERR_TIMED_OUT:
      return "ZX_ERR_TIMED_OUT";
    case ZX_ERR_SHOULD_WAIT:
      return "ZX_ERR_SHOULD_WAIT";
    case ZX_ERR_CANCELED:
      return "ZX_ERR_CANCELED";
    case ZX_ERR_PEER_CLOSED:
      return "ZX_ERR_PEER_CLOSED";
    case ZX_ERR_NOT_FOUND:
      return "ZX_ERR_NOT_FOUND";
    case ZX_ERR_ALREADY_EXISTS:
      return "ZX_ERR_ALREADY_EXISTS";
    case ZX_ERR_ALREADY_BOUND:
      return "ZX_ERR_ALREADY_BOUND";
    case ZX_ERR_UNAVAILABLE:
      return "ZX_ERR_UNAVAILABLE";
    case ZX_ERR_ACCESS_DENIED:
      return "ZX_ERR_ACCESS_DENIED";
    case ZX_ERR_IO:
      return "ZX_ERR_IO";
    case ZX_ERR_IO_REFUSED:
      return "ZX_ERR_IO_REFUSED";
    case ZX_ERR_IO_DATA_INTEGRITY:
      return "ZX_ERR_IO_DATA_INTEGRITY";
    case ZX_ERR_IO_DATA_LOSS:
      return "ZX_ERR_IO_DATA_LOSS";
    case ZX_ERR_IO_NOT_PRESENT:
      return "ZX_ERR_IO_NOT_PRESENT";
    case ZX_ERR_IO_OVERRUN:
      return "ZX_ERR_IO_OVERRUN";
    case ZX_ERR_IO_MISSED_DEADLINE:
      return "ZX_ERR_IO_MISSED_DEADLINE";
    case ZX_ERR_IO_INVALID:
      return "ZX_ERR_IO_INVALID";
    case ZX_ERR_BAD_PATH:
      return "ZX_ERR_BAD_PATH";
    case ZX_ERR_NOT_DIR:
      return "ZX_ERR_NOT_DIR";
    case ZX_ERR_NOT_FILE:
      return "ZX_ERR_NOT_FILE";
    case ZX_ERR_FILE_BIG:
      return "ZX_ERR_FILE_BIG";
    case ZX_ERR_NO_SPACE:
      return "ZX_ERR_NO_SPACE";
    case ZX_ERR_NOT_EMPTY:
      return "ZX_ERR_NOT_EMPTY";
    case ZX_ERR_STOP:
      return "ZX_ERR_STOP";
    case ZX_ERR_NEXT:
      return "ZX_ERR_NEXT";
    case ZX_ERR_ASYNC:
      return "ZX_ERR_ASYNC";
    case ZX_ERR_PROTOCOL_NOT_SUPPORTED:
      return "ZX_ERR_PROTOCOL_NOT_SUPPORTED";
    case ZX_ERR_ADDRESS_UNREACHABLE:
      return "ZX_ERR_ADDRESS_UNREACHABLE";
    case ZX_ERR_ADDRESS_IN_USE:
      return "ZX_ERR_ADDRESS_IN_USE";
    case ZX_ERR_NOT_CONNECTED:
      return "ZX_ERR_NOT_CONNECTED";
    case ZX_ERR_CONNECTION_REFUSED:
      return "ZX_ERR_CONNECTION_REFUSED";
    case ZX_ERR_CONNECTION_RESET:
      return "ZX_ERR_CONNECTION_RESET";
    case ZX_ERR_CONNECTION_ABORTED:
      return "ZX_ERR_CONNECTION_ABORTED";
    default:
      return "(UNKNOWN)";
  }
}

std::string ZxStatus_reprstr_helper(PyObject *self) {
  if (!PyObject_HasAttrString(self, "args")) {
    return "";
  }
  auto args = PyObject_GetAttrString(self, "args");
  if (PyTuple_Size(args) < 1) {
    Py_DECREF(args);
    return "unknown";
  }
  Py_DECREF(args);
  std::stringstream ss;
  PyObject *i = PyTuple_GetItem(args, 0);
  if (!PyLong_Check(i)) {
    return "unknown";
  }
  ss << zx_status_get_string(static_cast<zx_status_t>(PyLong_AsLong(i)));
  return ss.str();
}

PyObject *ZxStatus_repr(PyObject *self) {
  return PyUnicode_FromString(ZxStatus_reprstr_helper(self).c_str());
}

PyObject *ZxStatus_str(PyObject *self) {
  std::stringstream ss;
  ss << "FIDL status: " << ZxStatus_reprstr_helper(self);
  return PyUnicode_FromString(ss.str().c_str());
}

// This was copied and macro'd from the rust fuchsia_zircon_status files.
PyObject *ZxStatus_make_constants() {
  auto dict = PyDict_New();
  PyDict_SetItemString(dict, "ZX_OK", PyLong_FromLong(ZX_OK));
  PyDict_SetItemString(dict, "ZX_ERR_INTERNAL", PyLong_FromLong(ZX_ERR_INTERNAL));
  PyDict_SetItemString(dict, "ZX_ERR_NOT_SUPPORTED", PyLong_FromLong(ZX_ERR_NOT_SUPPORTED));
  PyDict_SetItemString(dict, "ZX_ERR_NO_RESOURCES", PyLong_FromLong(ZX_ERR_NO_RESOURCES));
  PyDict_SetItemString(dict, "ZX_ERR_NO_MEMORY", PyLong_FromLong(ZX_ERR_NO_MEMORY));
  PyDict_SetItemString(dict, "ZX_ERR_INVALID_ARGS", PyLong_FromLong(ZX_ERR_INVALID_ARGS));
  PyDict_SetItemString(dict, "ZX_ERR_BAD_HANDLE", PyLong_FromLong(ZX_ERR_BAD_HANDLE));
  PyDict_SetItemString(dict, "ZX_ERR_WRONG_TYPE", PyLong_FromLong(ZX_ERR_WRONG_TYPE));
  PyDict_SetItemString(dict, "ZX_ERR_BAD_SYSCALL", PyLong_FromLong(ZX_ERR_BAD_SYSCALL));
  PyDict_SetItemString(dict, "ZX_ERR_OUT_OF_RANGE", PyLong_FromLong(ZX_ERR_OUT_OF_RANGE));
  PyDict_SetItemString(dict, "ZX_ERR_BUFFER_TOO_SMALL", PyLong_FromLong(ZX_ERR_BUFFER_TOO_SMALL));
  PyDict_SetItemString(dict, "ZX_ERR_BAD_STATE", PyLong_FromLong(ZX_ERR_BAD_STATE));
  PyDict_SetItemString(dict, "ZX_ERR_TIMED_OUT", PyLong_FromLong(ZX_ERR_TIMED_OUT));
  PyDict_SetItemString(dict, "ZX_ERR_SHOULD_WAIT", PyLong_FromLong(ZX_ERR_SHOULD_WAIT));
  PyDict_SetItemString(dict, "ZX_ERR_CANCELED", PyLong_FromLong(ZX_ERR_CANCELED));
  PyDict_SetItemString(dict, "ZX_ERR_PEER_CLOSED", PyLong_FromLong(ZX_ERR_PEER_CLOSED));
  PyDict_SetItemString(dict, "ZX_ERR_NOT_FOUND", PyLong_FromLong(ZX_ERR_NOT_FOUND));
  PyDict_SetItemString(dict, "ZX_ERR_ALREADY_EXISTS", PyLong_FromLong(ZX_ERR_ALREADY_EXISTS));
  PyDict_SetItemString(dict, "ZX_ERR_ALREADY_BOUND", PyLong_FromLong(ZX_ERR_ALREADY_BOUND));
  PyDict_SetItemString(dict, "ZX_ERR_UNAVAILABLE", PyLong_FromLong(ZX_ERR_UNAVAILABLE));
  PyDict_SetItemString(dict, "ZX_ERR_ACCESS_DENIED", PyLong_FromLong(ZX_ERR_ACCESS_DENIED));
  PyDict_SetItemString(dict, "ZX_ERR_IO", PyLong_FromLong(ZX_ERR_IO));
  PyDict_SetItemString(dict, "ZX_ERR_IO_REFUSED", PyLong_FromLong(ZX_ERR_IO_REFUSED));
  PyDict_SetItemString(dict, "ZX_ERR_IO_DATA_INTEGRITY", PyLong_FromLong(ZX_ERR_IO_DATA_INTEGRITY));
  PyDict_SetItemString(dict, "ZX_ERR_IO_DATA_LOSS", PyLong_FromLong(ZX_ERR_IO_DATA_LOSS));
  PyDict_SetItemString(dict, "ZX_ERR_IO_NOT_PRESENT", PyLong_FromLong(ZX_ERR_IO_NOT_PRESENT));
  PyDict_SetItemString(dict, "ZX_ERR_IO_OVERRUN", PyLong_FromLong(ZX_ERR_IO_OVERRUN));
  PyDict_SetItemString(dict, "ZX_ERR_IO_MISSED_DEADLINE",
                       PyLong_FromLong(ZX_ERR_IO_MISSED_DEADLINE));
  PyDict_SetItemString(dict, "ZX_ERR_IO_INVALID", PyLong_FromLong(ZX_ERR_IO_INVALID));
  PyDict_SetItemString(dict, "ZX_ERR_BAD_PATH", PyLong_FromLong(ZX_ERR_BAD_PATH));
  PyDict_SetItemString(dict, "ZX_ERR_NOT_DIR", PyLong_FromLong(ZX_ERR_NOT_DIR));
  PyDict_SetItemString(dict, "ZX_ERR_NOT_FILE", PyLong_FromLong(ZX_ERR_NOT_FILE));
  PyDict_SetItemString(dict, "ZX_ERR_FILE_BIG", PyLong_FromLong(ZX_ERR_FILE_BIG));
  PyDict_SetItemString(dict, "ZX_ERR_NO_SPACE", PyLong_FromLong(ZX_ERR_NO_SPACE));
  PyDict_SetItemString(dict, "ZX_ERR_NOT_EMPTY", PyLong_FromLong(ZX_ERR_NOT_EMPTY));
  PyDict_SetItemString(dict, "ZX_ERR_STOP", PyLong_FromLong(ZX_ERR_STOP));
  PyDict_SetItemString(dict, "ZX_ERR_NEXT", PyLong_FromLong(ZX_ERR_NEXT));
  PyDict_SetItemString(dict, "ZX_ERR_ASYNC", PyLong_FromLong(ZX_ERR_ASYNC));
  PyDict_SetItemString(dict, "ZX_ERR_PROTOCOL_NOT_SUPPORTED",
                       PyLong_FromLong(ZX_ERR_PROTOCOL_NOT_SUPPORTED));
  PyDict_SetItemString(dict, "ZX_ERR_ADDRESS_UNREACHABLE",
                       PyLong_FromLong(ZX_ERR_ADDRESS_UNREACHABLE));
  PyDict_SetItemString(dict, "ZX_ERR_ADDRESS_IN_USE", PyLong_FromLong(ZX_ERR_ADDRESS_IN_USE));
  PyDict_SetItemString(dict, "ZX_ERR_NOT_CONNECTED", PyLong_FromLong(ZX_ERR_NOT_CONNECTED));
  PyDict_SetItemString(dict, "ZX_ERR_CONNECTION_REFUSED",
                       PyLong_FromLong(ZX_ERR_CONNECTION_REFUSED));
  PyDict_SetItemString(dict, "ZX_ERR_CONNECTION_RESET", PyLong_FromLong(ZX_ERR_CONNECTION_RESET));
  PyDict_SetItemString(dict, "ZX_ERR_CONNECTION_ABORTED",
                       PyLong_FromLong(ZX_ERR_CONNECTION_ABORTED));
  return dict;
}

PyTypeObject *ZxStatusType = nullptr;

// This is necessary as Python exception extensions in C are weird. Python exceptions can't be
// wholesale subclassed from the base exception class, as they are expected to have an args
// attribute from which they are constructed. Attempting to make any sort of non-trivial subclassing
// of the Python error subclass will result in segfaulting.
PyTypeObject *ZxStatusType_Create() {
  assert(ZxStatusType == nullptr);
  auto res = reinterpret_cast<PyTypeObject *>(
      PyErr_NewException("fuchsia_controller_py.ZxStatus", nullptr, ZxStatus_make_constants()));
  res->tp_repr = ZxStatus_repr;
  res->tp_str = ZxStatus_str;
  ZxStatusType = res;
  Py_INCREF(res);
  return res;
}

}  // namespace error

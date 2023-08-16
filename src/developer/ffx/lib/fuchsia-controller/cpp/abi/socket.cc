// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "socket.h"

#include "convert.h"
#include "error.h"
#include "mod.h"
#include "src/developer/ffx/lib/fuchsia-controller/cpp/raii/py_wrapper.h"

namespace socket {

int Socket_init(Socket *self, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"handle", nullptr};
  PyObject *handle = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "O", const_cast<char **>(kwlist), &handle)) {
    return -1;
  }
  bool is_handle = PyObject_IsInstance(handle, reinterpret_cast<PyObject *>(&handle::HandleType));
  bool is_long = PyLong_Check(handle);
  if (!is_handle && !is_long) {
    PyErr_SetString(PyExc_TypeError,
                    "Expected 'handle' to be either a fuchsia_controller_py.Handle type or an int");
    return -1;
  }

  // Might be a bit too much checking, but just make sure being a long and a handle are
  // actually mutually exclusive of each other.
  assert(is_long != is_handle);
  if (is_long) {
    zx_handle_t converted_handle = convert::PyLong_AsU32(handle);
    if (converted_handle == convert::MINUS_ONE_U32 && PyErr_Occurred()) {
      return -1;
    }
    self->super.handle = converted_handle;
  }
  if (is_handle) {
    auto f_handle = reinterpret_cast<handle::Handle *>(handle);
    self->super.handle = f_handle->handle;
    // Nullifies the previous handle, preventing it from closing this socket.
    f_handle->handle = 0;
  }
  return 0;
}

// First arg in this function is SocketType.
PyObject *Socket_create(PyObject *cls, PyObject *args, PyObject *kwds) {
  static const char *kwlist[] = {"options", nullptr};
  uint32_t *options = nullptr;
  if (!PyArg_ParseTupleAndKeywords(args, kwds, "|I", const_cast<char **>(kwlist), &options)) {
    return nullptr;
  }

  const uint32_t socket_opts = options ? *options : 0;
  zx_handle_t hdl0;
  zx_handle_t hdl1;
  zx_status_t create_status =
      ffx_socket_create(mod::get_module_state()->ctx, socket_opts, &hdl0, &hdl1);
  if (create_status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType),
                    PyLong_FromLong(create_status));
    return nullptr;
  }
  py::Object tuple(PyTuple_New(2));
  if (tuple == nullptr) {
    return nullptr;
  }
  py::Object handle_obj0(PyObject_CallFunction(cls, "I", hdl0));
  if (handle_obj0 == nullptr) {
    return nullptr;
  }
  py::Object handle_obj1(PyObject_CallFunction(cls, "I", hdl1));
  if (handle_obj1 == nullptr) {
    return nullptr;
  }
  PyTuple_SET_ITEM(tuple.get(), 0, handle_obj0.take());
  PyTuple_SET_ITEM(tuple.get(), 1, handle_obj1.take());
  return tuple.take();
}

PyObject *Socket_read(Socket *self, PyObject *Py_UNUSED(arg)) {
  constexpr static uint64_t c_buf_len = 65536;
  static char c_buf[c_buf_len] = {};
  uint64_t bytes_read = 0;

  auto status = ffx_socket_read(mod::get_module_state()->ctx, self->super.handle, c_buf, c_buf_len,
                                &bytes_read);
  if (status != ZX_OK) {
    static_assert(sizeof(long) >= sizeof(status));  // NOLINT
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }

  auto buf = PyByteArray_FromStringAndSize(const_cast<const char *>(c_buf),
                                           static_cast<Py_ssize_t>(bytes_read));
  return buf;
}

PyObject *Socket_write(Socket *self, PyObject *buffer) {
  if (buffer == nullptr || !PyObject_CheckBuffer(buffer)) {
    PyErr_SetString(PyExc_TypeError, "Expected a non-null buffer object");
    return nullptr;
  }
  Py_buffer view;
  if (PyObject_GetBuffer(buffer, &view, PyBUF_CONTIG_RO) < 0) {
    return nullptr;
  }

  auto status =
      ffx_socket_write(mod::get_module_state()->ctx, self->super.handle,
                       reinterpret_cast<const char *>(view.buf), static_cast<uint64_t>(view.len));
  PyBuffer_Release(&view);  // Done with write buffer; always release
  if (status != ZX_OK) {
    PyErr_SetObject(reinterpret_cast<PyObject *>(error::ZxStatusType), PyLong_FromLong(status));
    return nullptr;
  }

  Py_RETURN_NONE;
}

PyObject *Socket_as_int(Socket *self, PyObject *Py_UNUSED(arg)) {
  return PyLong_FromUnsignedLongLong(self->super.handle);
}

PyObject *Socket_take(Socket *self, PyObject *Py_UNUSED(arg)) {
  auto result = PyLong_FromUnsignedLongLong(self->super.handle);
  self->super.handle = 0;
  return result;
}

PyMethodDef Socket_methods[] = {
    {"create", reinterpret_cast<PyCFunction>(Socket_create), METH_CLASS | METH_VARARGS,
     "classmethod for creating a pair of sockets. These are connected bidirectionally."},
    {"read", reinterpret_cast<PyCFunction>(Socket_read), METH_NOARGS, nullptr},
    {"write", reinterpret_cast<PyCFunction>(Socket_write), METH_O, nullptr},
    {"as_int", reinterpret_cast<PyCFunction>(Socket_as_int), METH_NOARGS, nullptr},
    {"take", reinterpret_cast<PyCFunction>(Socket_take), METH_NOARGS,
     "Takes the underlying fidl handle, setting it internally to zero (thus invalidating the "
     "underlying socket). This is used for sending a handle through FIDL function calls."},
    {nullptr, nullptr, 0, nullptr}};

DES_MIX PyTypeObject SocketType = {
    PyVarObject_HEAD_INIT(nullptr, 0).tp_name = "fuchsia_controller_py.Socket",
    .tp_flags = Py_TPFLAGS_DEFAULT,
    .tp_doc =
        "Fuchsia controller Zircon socket. This can be read from and written to.\n"
        "\n"
        "Can be constructed from a Handle object, but keep in mind that this will mark\n"
        "the caller's handle invalid, leaving this socket to be the only owner of the underlying\n"
        "handle.",
    .tp_methods = Socket_methods,
    .tp_base = &handle::HandleType,
    .tp_init = reinterpret_cast<initproc>(Socket_init),
};

}  // namespace socket

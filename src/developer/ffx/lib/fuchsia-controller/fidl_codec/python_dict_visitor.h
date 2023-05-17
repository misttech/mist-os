// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_PYTHON_DICT_VISITOR_H_
#define SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_PYTHON_DICT_VISITOR_H_
#include <Python.h>

#include <iostream>
#include <string>

#include "py_wrapper.h"
#include "src/developer/ffx/lib/fuchsia-controller/abi/convert.h"
#include "src/lib/fidl_codec/printer.h"
#include "src/lib/fidl_codec/visitor.h"

namespace python_dict_visitor {

class PythonDictVisitor : public fidl_codec::Visitor {
 public:
  PythonDictVisitor() = default;
  PyObject* result() { return result_; }

 private:
  void VisitValue(const fidl_codec::Value* node, const fidl_codec::Type* for_type) override {
    std::stringstream ss;
    fidl_codec::PrettyPrinter printer(ss, fidl_codec::WithoutColors, false, "", 0, false);
    node->PrettyPrint(for_type, printer);
    result_ = PyUnicode_FromString(ss.str().c_str());
  }

  void VisitInvalidValue(const fidl_codec::InvalidValue* node,
                         const fidl_codec::Type* for_type) override {
    PyErr_SetString(PyExc_TypeError, "invalid value");
  }

  void VisitNullValue(const fidl_codec::NullValue* node,
                      const fidl_codec::Type* for_type) override {
    Py_IncRef(Py_None);
    result_ = Py_None;
  }

  void VisitBoolValue(const fidl_codec::BoolValue* node,
                      const fidl_codec::Type* for_type) override {
    result_ = PyBool_FromLong(node->value());
  }

  void VisitStringValue(const fidl_codec::StringValue* node,
                        const fidl_codec::Type* for_type) override {
    result_ = PyUnicode_FromString(node->string().c_str());
  }

  void VisitUnionValue(const fidl_codec::UnionValue* node, const fidl_codec::Type* type) override {
    auto res = py::Object(PyDict_New());
    auto key = py::Object(PyUnicode_FromString(node->member().name().c_str()));
    if (key == nullptr) {
      return;
    }
    PythonDictVisitor visitor;
    node->value()->Visit(&visitor, node->member().type());
    if (visitor.result() == nullptr) {
      return;
    }
    PyDict_SetItem(res.get(), key.take(), visitor.result());
    result_ = res.take();
  }

  void VisitStructValue(const fidl_codec::StructValue* node,
                        const fidl_codec::Type* for_type) override {
    auto res = py::Object(PyDict_New());
    for (const auto& member : node->struct_definition().members()) {
      auto it = node->fields().find(member.get());
      if (it == node->fields().end()) {
        continue;
      }
      auto key = py::Object(PyUnicode_FromString(member->name().c_str()));
      PythonDictVisitor visitor;
      it->second->Visit(&visitor, member->type());
      if (visitor.result() == nullptr) {
        return;
      }
      PyDict_SetItem(res.get(), key.take(), visitor.result());
    }
    result_ = res.take();
  }

  void VisitVectorValue(const fidl_codec::VectorValue* node,
                        const fidl_codec::Type* for_type) override {
    if (for_type == nullptr) {
      PyErr_SetString(PyExc_TypeError,
                      "expected vector type in during decoding. Received null value");
      return;
    }
    const auto component_type = for_type->GetComponentType();
    if (component_type == nullptr) {
      PyErr_SetString(PyExc_TypeError, "vector value's type does not contain a component type");
      return;
    }
    auto res = py::Object(PyList_New(static_cast<Py_ssize_t>(node->values().size())));
    Py_ssize_t values_size = static_cast<Py_ssize_t>(node->values().size());
    const auto& values = node->values();
    for (Py_ssize_t i = 0; i < values_size; ++i) {
      PythonDictVisitor visitor;
      values[i]->Visit(&visitor, component_type);
      if (visitor.result() == nullptr) {
        return;
      }
      PyList_SetItem(res.get(), i, visitor.result());
    }
    result_ = res.take();
  }

  void VisitTableValue(const fidl_codec::TableValue* node,
                       const fidl_codec::Type* for_type) override {
    auto res = py::Object(PyDict_New());
    for (const auto& member : node->table_definition().members()) {
      if ((member != nullptr) && !member->reserved()) {
        auto it = node->members().find(member.get());
        if (it == node->members().end()) {
          continue;
        }
        auto key = py::Object(PyUnicode_FromString(member->name().c_str()));
        if (key == nullptr) {
          return;
        }
        if (it->second == nullptr || it->second->IsNull()) {
          Py_IncRef(Py_None);
          PyDict_SetItem(res.get(), key.take(), Py_None);
          continue;
        }
        PythonDictVisitor visitor;
        it->second->Visit(&visitor, member->type());
        if (visitor.result() == nullptr) {
          return;
        }
        PyDict_SetItem(res.get(), key.take(), visitor.result());
      }
    }
    result_ = res.take();
  }

  void VisitDoubleValue(const fidl_codec::DoubleValue* node,
                        const fidl_codec::Type* for_type) override {
    double value;
    node->GetDoubleValue(&value);
    result_ = PyFloat_FromDouble(value);
  }

  void VisitIntegerValue(const fidl_codec::IntegerValue* node,
                         const fidl_codec::Type* for_type) override {
    uint64_t value;
    bool negative;
    node->GetIntegerValue(&value, &negative);
    if (negative) {
      // Max possible absolute value for a signed integer (2^63 - 1).
      if (value > 0x7fffffffffffffff) {
        std::stringstream ss;
        PyErr_SetString(PyExc_OverflowError, "Integer overflow");
        return;
      }
      int64_t res = static_cast<int64_t>(value);
      res *= -1;
      result_ = PyLong_FromLongLong(res);
    } else {
      result_ = PyLong_FromUnsignedLongLong(value);
    }
  }

  void VisitHandleValue(const fidl_codec::HandleValue* handle,
                        const fidl_codec::Type* for_type) override {
    // TODO(fxbug.dev/124288): Use the handle disposition properly. This is leaving it
    // mostly blank, only copying the handle value explicitly.
    result_ = PyLong_FromUnsignedLong(handle->handle().handle);
  }

  PyObject* result_{nullptr};
};

}  // namespace python_dict_visitor
#endif  // SRC_DEVELOPER_FFX_LIB_FUCHSIA_CONTROLLER_FIDL_CODEC_PYTHON_DICT_VISITOR_H_

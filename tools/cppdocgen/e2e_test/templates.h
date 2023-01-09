// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef TOOLS_CPPDOCGEN_E2E_TEST_TEMPLATES_H_
#define TOOLS_CPPDOCGEN_E2E_TEST_TEMPLATES_H_

// This file has some weird formatting on purpose.
// clang-format off

// A base template type.
template <typename T, int i = 1>
class BaseTemplate {
 public:
  T GetValue() { return static_cast<T>(0); }
};

// Partial template specialization.
template <typename T>
class BaseTemplate<T, 0> {};

// Full template specialization.
template <>
class BaseTemplate<int, 0> {
 public:
  int TemplateFunctionOnBase();
};

// Class derived from a template.
class DerivesFromTemplate : public BaseTemplate<int, 0> {};

// Templatized function
template <class
          T /*
          Some comment formatted weirdly */
          ,
          int s  // something
          >
void TemplateFunction(T t) {}

// Templatized function specialization.
template <>
void TemplateFunction<double, 0>(double) {}

#endif  // TOOLS_CPPDOCGEN_E2E_TEST_TEMPLATES_H_

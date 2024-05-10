// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/test.sagcontrol/cpp/fidl.h>
#include <fidl/test.sagcontrol/cpp/natural_ostream.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <zxtest/zxtest.h>

template <typename T>
std::string ToString(const T& value) {
  std::ostringstream buf;
  buf << value;
  return buf.str();
}
template <typename T>
std::string FidlString(const T& value) {
  return ToString(fidl::ostream::Formatted<T>(value));
}

TEST(PowerSystemIntegration, ExampleTest) {
  // Can also connect to /dev to access the suspend and storage drivers.
  auto result = component::Connect<test_sagcontrol::State>();
  ASSERT_EQ(ZX_OK, result.status_value());
  auto get_result = fidl::Call(result.value())->Get();
  std::cout << FidlString(get_result.value()) << std::endl;
}

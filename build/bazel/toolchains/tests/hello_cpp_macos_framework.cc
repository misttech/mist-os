// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// A trivial program to verify that linking against a system framework works.
#include <iostream>

#include <CoreFoundation/CoreFoundation.h>

int main() {
  CFLocaleGetSystem();
  std::cout << "Success" << std::endl;
  return 0;
}

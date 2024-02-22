// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.#include <lib/ddk/binding_driver.h>

#include "hello_printer_17.h"

#include <iostream>

namespace hello_printer_17 {

void HelloPrinter::PrintHello() { std::cout << "Hello, from API level 17!\n"; }
}  // namespace hello_printer_17

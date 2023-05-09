// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>

namespace {
int SomeGlobal = 0;

void LeafNoArgs() {
  // Create a local variable so something happens to the stack.
  volatile int i = 10;
  SomeGlobal = i;
}

void NestedNoArgs() { LeafNoArgs(); }

void NestedTwiceNoArgs() { NestedNoArgs(); }

void PrintHello() { std::cout << "Hello! SomeGlobal = " << SomeGlobal << std::endl; }

}  // namespace

int main() {
  NestedTwiceNoArgs();
  PrintHello();
}

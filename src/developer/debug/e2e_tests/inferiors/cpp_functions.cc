// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>

namespace {
int SomeGlobal = 0;
constexpr char kHello[] = "Hello!";

void LeafNoArgs() {
  // Create a local variable so something happens to the stack.
  volatile int i = 10;
  SomeGlobal = i;
}

void NestedNoArgs() { LeafNoArgs(); }

void NestedTwiceNoArgs() { NestedNoArgs(); }

int ReturnGlobalPlusOne() { return SomeGlobal + 1; }

int* GetIntPointer() { return &SomeGlobal; }

float GetFloat() { return 3.14159f; }

const char* GetCharPtr() { return kHello; }

void PrintHello() { std::cout << "Hello! SomeGlobal = " << SomeGlobal << std::endl; }

}  // namespace

int main() {
  NestedTwiceNoArgs();
  PrintHello();
  std::cout << ReturnGlobalPlusOne() << "\n";
  std::cout << *GetIntPointer() << "\n";
  std::cout << GetFloat() << "\n";
  std::cout << GetCharPtr() << "\n";
}

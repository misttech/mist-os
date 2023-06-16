// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <string_view>
#include <unordered_set>

class Parent {
 public:
  virtual ~Parent() = default;

  int a = 0;
};

class Child : public Parent {
 public:
  int b = 1;
};

int main() {
  std::shared_ptr<Parent> p = std::make_shared<Child>();
  std::string_view sv = "abc";
  std::map<std::string, int> map = {{"a", 1}, {"b", 2}};
  std::unordered_set<int> set = {0, 1, 2};

  std::cout << sv << p->a;
}

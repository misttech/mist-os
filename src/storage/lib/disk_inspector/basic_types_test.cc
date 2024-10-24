// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Tests disk-inspector primitive data type behavior.

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>

#include <gtest/gtest.h>

#include "src/storage/lib/disk_inspector/common_types.h"
#include "src/storage/lib/disk_inspector/disk_inspector.h"

namespace disk_inspector {
namespace {

void TestPrimitiveType(DiskObject* obj, const void* expected_value, size_t expected_size) {
  ASSERT_EQ(0u, obj->GetNumElements());
  ASSERT_EQ(nullptr, obj->GetElementAt(0).get());
  ASSERT_EQ(nullptr, obj->GetElementAt(1).get());

  size_t size;
  const void* buffer = nullptr;

  obj->GetValue(&buffer, &size);
  ASSERT_EQ(size, expected_size);

  if (expected_size == sizeof(uint32_t)) {
    const char name[] = "uint32Obj";
    auto out_name = obj->GetName();
    ASSERT_STREQ(out_name, name);
    const uint32_t* value = reinterpret_cast<const uint32_t*>(buffer);
    ASSERT_EQ(*value, *(reinterpret_cast<const uint32_t*>(expected_value)));
  } else if (expected_size == sizeof(uint64_t)) {
    const char name[] = "uint64Obj";
    auto out_name = obj->GetName();
    ASSERT_STREQ(out_name, name);
    const uint64_t* value = reinterpret_cast<const uint64_t*>(buffer);
    ASSERT_EQ(*value, *(reinterpret_cast<const uint64_t*>(expected_value)));
  } else if (expected_size == sizeof(char)) {
    const char name[] = "charObj";
    auto out_name = obj->GetName();
    ASSERT_STREQ(out_name, name);
    const char* value = reinterpret_cast<const char*>(buffer);
    ASSERT_EQ(*value, *(reinterpret_cast<const char*>(expected_value)));
  } else {
    FAIL();
  }
}

TEST(DiskInspectorTest, TestUint32) {
  uint32_t inp_val = 5;
  auto out_obj = std::make_unique<DiskObjectUint32>("uint32Obj", &inp_val);

  TestPrimitiveType(out_obj.get(), &inp_val, sizeof(uint32_t));
}

TEST(DiskInspectorTest, TestUint64) {
  uint64_t inp_val = 55;
  auto out_obj = std::make_unique<DiskObjectUint64>("uint64Obj", &inp_val);

  TestPrimitiveType(out_obj.get(), &inp_val, sizeof(uint64_t));
}

TEST(DiskInspectorTest, TestChar) {
  char inp_val = 'h';
  auto out_obj = std::make_unique<DiskObjectChar>("charObj", &inp_val);

  TestPrimitiveType(out_obj.get(), &inp_val, sizeof(char));
}

TEST(DiskInspectorTest, TestUint32Array) {
  uint32_t inp_arr[2] = {1, 2};
  auto out_obj = std::make_unique<DiskObjectUint32Array>("uint32Obj", inp_arr, 2);

  ASSERT_EQ(out_obj->GetName(), std::string("uint32Obj"));
  ASSERT_EQ(2u, out_obj->GetNumElements());

  // Testing element at index 0
  std::unique_ptr<DiskObject> elem_obj = out_obj->GetElementAt(0);
  ASSERT_NE(nullptr, elem_obj.get());
  TestPrimitiveType(elem_obj.get(), inp_arr, sizeof(inp_arr[0]));

  // Testing element at index 1
  std::unique_ptr<DiskObject> elem_obj1 = out_obj->GetElementAt(1);
  ASSERT_NE(nullptr, elem_obj1.get());
  TestPrimitiveType(elem_obj1.get(), &inp_arr[1], sizeof(inp_arr[1]));

  // Check out of range elements
  ASSERT_EQ(nullptr, out_obj->GetElementAt(2).get());
}

TEST(DiskInspectorTest, TestUint64Array) {
  uint64_t inp_arr[2] = {7, 8};
  auto out_obj = std::make_unique<DiskObjectUint64Array>("uint64Obj", inp_arr, 2);

  ASSERT_EQ(out_obj->GetName(), std::string("uint64Obj"));
  ASSERT_EQ(2u, out_obj->GetNumElements());

  // Testing element at index 0
  std::unique_ptr<DiskObject> elem_obj = out_obj->GetElementAt(0);
  ASSERT_NE(nullptr, elem_obj.get());
  TestPrimitiveType(elem_obj.get(), inp_arr, sizeof(inp_arr[0]));

  // Testing element at index 1
  std::unique_ptr<DiskObject> elem_obj1 = out_obj->GetElementAt(1);
  ASSERT_NE(nullptr, elem_obj1.get());
  TestPrimitiveType(elem_obj1.get(), &inp_arr[1], sizeof(inp_arr[1]));

  // Check out of range elements
  ASSERT_EQ(nullptr, out_obj->GetElementAt(2).get());
}

TEST(DiskInspectorTest, TestCharArray) {
  char inp_arr[2] = {'h', 'i'};
  auto out_obj = std::make_unique<DiskObjectCharArray>("charObj", inp_arr, 2);

  ASSERT_EQ(out_obj->GetName(), std::string("charObj"));
  ASSERT_EQ(2u, out_obj->GetNumElements());

  // Testing element at index 0
  std::unique_ptr<DiskObject> elem_obj = out_obj->GetElementAt(0);
  ASSERT_NE(nullptr, elem_obj.get());
  TestPrimitiveType(elem_obj.get(), inp_arr, sizeof(inp_arr[0]));

  // Testing element at index 1
  std::unique_ptr<DiskObject> elem_obj1 = out_obj->GetElementAt(1);
  ASSERT_NE(nullptr, elem_obj1.get());
  TestPrimitiveType(elem_obj1.get(), &inp_arr[1], sizeof(inp_arr[1]));

  // Check out of range elements
  ASSERT_EQ(nullptr, out_obj->GetElementAt(2).get());
}

}  // namespace
}  // namespace disk_inspector

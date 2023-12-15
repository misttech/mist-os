// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zxio/ops.h>
#include <string.h>

#include <zxtest/zxtest.h>

TEST(OpsTest, Close) {
  zxio_ops_t ops;
  memset(&ops, 0, sizeof(ops));
  ops.close = [](zxio_t*, bool) { return ZX_OK; };

  zxio_t io = {};
  ASSERT_EQ(nullptr, zxio_get_ops(&io));

  zxio_init(&io, &ops);

  ASSERT_EQ(&ops, zxio_get_ops(&io));
  ASSERT_OK(zxio_close(&io, /*should_wait=*/true));
}

TEST(OpsTest, CloseWillInvalidateTheObject) {
  zxio_ops_t ops;
  memset(&ops, 0, sizeof(ops));
  ops.close = [](zxio_t*, bool) { return ZX_OK; };

  zxio_t io = {};
  zxio_init(&io, &ops);
  ASSERT_OK(zxio_close(&io, /*should_wait=*/true));
  ASSERT_STATUS(zxio_close(&io, /*should_wait=*/true), ZX_ERR_BAD_HANDLE);
  ASSERT_STATUS(zxio_release(&io, nullptr), ZX_ERR_BAD_HANDLE);
}

TEST(AttributesTest, RootHashEquality) {
  uint8_t root_hash[ZXIO_ROOT_HASH_LENGTH];
  memset(&root_hash, 1, ZXIO_ROOT_HASH_LENGTH);
  zxio_node_attributes_t attr = {
      .fsverity_root_hash = &root_hash[0],
      .has =
          {
              .fsverity_root_hash = true,
          },
  };

  uint8_t expected_root_hash[ZXIO_ROOT_HASH_LENGTH];
  memset(&expected_root_hash, 1, ZXIO_ROOT_HASH_LENGTH);
  zxio_node_attributes_t expected_attr = {
      .fsverity_root_hash = &expected_root_hash[0],
      .has =
          {
              .fsverity_root_hash = true,
          },
  };
  ASSERT_EQ(attr, expected_attr);
}

TEST(AttributesTest, RootHashNullptr) {
  uint8_t root_hash[ZXIO_ROOT_HASH_LENGTH];
  memset(&root_hash, 1, ZXIO_ROOT_HASH_LENGTH);
  zxio_node_attributes_t attr = {
      .fsverity_root_hash = &root_hash[0],
      .has =
          {
              .fsverity_root_hash = true,
          },
  };

  zxio_node_attributes_t expected_attr = {
      .fsverity_root_hash = nullptr,
      .has =
          {
              .fsverity_root_hash = true,
          },
  };
  ASSERT_FALSE(attr == expected_attr);
}

TEST(AttributesTest, IdenticalSaltDifferentSaltSize) {
  uint8_t salt[32];
  memset(&salt, 1, 8);
  zxio_verification_options_t options;
  memcpy(options.salt, salt, 8);
  options.salt_size = 8;
  options.hash_alg = 1;
  zxio_node_attributes_t attr = {
      .fsverity_options = options,
      .has =
          {
              .fsverity_options = true,
          },
  };

  zxio_verification_options_t expected_options;
  memcpy(expected_options.salt, salt, 8);
  expected_options.salt_size = 32;
  expected_options.hash_alg = 1;

  zxio_node_attributes_t expected_attr = {
      .fsverity_options = expected_options,
      .has =
          {
              .fsverity_options = true,
          },
  };

  ASSERT_FALSE(attr == expected_attr);
}

TEST(AttributesTest, SameSaltSizeDifferentSalt) {
  uint8_t salt[32];
  memset(&salt, 1, 8);
  zxio_verification_options_t options;
  memcpy(options.salt, salt, 8);
  options.salt_size = 8;
  options.hash_alg = 1;
  zxio_node_attributes_t attr = {
      .fsverity_options = options,
      .has =
          {
              .fsverity_options = true,
          },
  };

  zxio_verification_options_t expected_options;
  memset(&salt, 2, 8);
  memcpy(expected_options.salt, salt, 8);
  expected_options.salt_size = 8;
  expected_options.hash_alg = 1;

  zxio_node_attributes_t expected_attr = {
      .fsverity_options = expected_options,
      .has =
          {
              .fsverity_options = true,
          },
  };

  ASSERT_FALSE(attr == expected_attr);
}

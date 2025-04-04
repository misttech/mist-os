// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <zxtest/zxtest.h>

#define THREAD_COUNT 8
#define ITER 1000000

#define ARRAY_SIZE(x) (sizeof(x) / sizeof(*(x)))

/* expected double bit pattern for each thread */
static const uint64_t expected[THREAD_COUNT] = {
    0x4284755ed4188b3e, 0x4284755ed6cb84c0, 0x4284755ed97e7dd3, 0x4284755edc317770,
    0x4284755edee471b9, 0x4284755ee1976c19, 0x4284755ee44a648b, 0x4284755ee6fd5fa7,
};

/* optimize this function to cause it to try to use a lot of registers */
__OPTIMIZE("O3")
static int float_thread(void* arg) {
  double* val = arg;
  unsigned int i, j;
  double a[16];

  printf("float_thread arg %f, running %u iterations\n", *val, ITER);
  usleep(500000);

  /* do a bunch of work with floating point to test context switching */
  a[0] = *val;
  for (i = 1; i < ARRAY_SIZE(a); i++) {
    a[i] = a[i - 1] * 1.01;
  }

  for (i = 0; i < ITER; i++) {
    a[0] += i;
    for (j = 1; j < ARRAY_SIZE(a); j++) {
      a[j] += a[j - 1] * 0.00001;
    }
  }

  *val = a[ARRAY_SIZE(a) - 1];
  return 0;
}

TEST(FpuTests, fpu_test) {
  printf("welcome to floating point test\n");

  /* test lazy fpu load on separate thread */
  thrd_t t[THREAD_COUNT];
  double val[ARRAY_SIZE(t)];
  char name[ZX_MAX_NAME_LEN];

  printf("creating %zu floating point threads\n", ARRAY_SIZE(t));
  for (unsigned int i = 0; i < ARRAY_SIZE(t); i++) {
    val[i] = i;
    snprintf(name, sizeof(name), "fpu thread %u", i);
    thrd_create_with_name(&t[i], float_thread, &val[i], name);
  }

  for (unsigned int i = 0; i < ARRAY_SIZE(t); i++) {
    thrd_join(t[i], NULL);
    void* v = &val[i];
    uint64_t int64_val = *(uint64_t*)v;

    printf("float thread %u returns val %f %#" PRIx64 ", expected %#" PRIx64 "\n", i, val[i],
           int64_val, expected[i]);
    EXPECT_EQ(int64_val, expected[i], "Value does not match as expected");
  }

  printf("floating point test done\n");
}

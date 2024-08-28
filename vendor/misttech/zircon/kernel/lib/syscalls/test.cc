// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>

long sys_test_0() { return 0; }
long sys_test_1(int a) { return a; }
long sys_test_2(int a, int b) { return a + b; }
long sys_test_3(int a, int b, int c) { return a + b + c; }
long sys_test_4(int a, int b, int c, int d) { return a + b + c + d; }
long sys_test_5(int a, int b, int c, int d, int e) { return a + b + c + d + e; }
long sys_test_6(int a, int b, int c, int d, int e, int f) { return a + b + c + d + e + f; }

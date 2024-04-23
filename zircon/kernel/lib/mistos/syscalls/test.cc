// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/syscalls/forward.h>

// long zx_syscall_test_0
long sys_syscall_test_0(void) { return 0; }
// long zx_syscall_test_1
long sys_syscall_test_1(int a) { return a; }
// long zx_syscall_test_2
long sys_syscall_test_2(int a, int b) { return a + b; }
// long zx_syscall_test_3
long sys_syscall_test_3(int a, int b, int c) { return a + b + c; }
// long zx_syscall_test_4
long sys_syscall_test_4(int a, int b, int c, int d) { return a + b + c + d; }
// long zx_syscall_test_5
long sys_syscall_test_5(int a, int b, int c, int d, int e) { return a + b + c + d + e; }
// long zx_syscall_test_6
long sys_syscall_test_6(int a, int b, int c, int d, int e, int f) { return a + b + c + d + e + f; }

#!/bin/bash
# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

#set -x
#set -e

#This script is WIP, just to give the directions not yet fully functional.

#Download linux source code to <linux_root_dir>

#cp <mistos_root_dir>/examples/nolibc/nolibc_kernel.config  <linux_root_dir>/.config
#cd <linux_root_dir> 
#make V=1 ARCH=x86_64 CROSS_COMPILE=x86_64-elf- \
#HOSTCFLAGS="-I/Volumes/unikernel/Projects/mac-linux-kdk/env/include/ \ 
#-D_UUID_T -D__GETHOSTUUID_H" AFLAGS_KERNEL="-Wa,-divide" \
#-C tools/testing/selftests/nolibc all

#cp <linux_root_dir>/tools/testing/selftests/nolibc/nolibc-test <mistos_root_dir>/examples/nolibc/nolibc-test.linux

#cat include/nolibc/arch-x86_64.h > include/nolibc/arch.h
#sed -e 's,^#ifndef _NOLIBC_ARCH_X86_64_H,#if !defined(_NOLIBC_ARCH_X86_64_H) \&\& defined(__x86_64__),' include/nolibc/arch-x86_64.h > include/nolibc/arch.h
#sed -e 's,^#ifndef _NOLIBC_ARCH_I386_H,#if !defined(_NOLIBC_ARCH_I386_H) \&\& !defined(__x86_64__),' include/nolibc/arch-i386.h >> include/nolibc/arch.h
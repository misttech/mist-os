#!/bin/bash
# Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

zircon/scripts/run-zircon-x64 -t out/default/multiboot.bin -z ./out/default/obj/examples/nolibc/nolibc.zbi \
-c "userboot.test.next=test/nolibc-test+syscall:10-11,stdlib kernel.bypass-debuglog=true" -s1 -- -no-reboot > "./out/default/obj/examples/nolibc/run.out"
grep -w FAIL "./out/default/obj/examples/nolibc/run.out" && \
echo "See all results in ./out/default/obj/examples/nolibc/run.out" || \
echo "$(grep -c ^[0-9].*OK ./out/default/obj/examples/nolibc/run.out) test(s) passed."
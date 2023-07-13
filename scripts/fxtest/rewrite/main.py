# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio


def main():
    asyncio.run(async_main())


async def async_main():
    print("Welcome to the new fx test")


if __name__ == "__main__":
    main()

// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include "heapdump/bind_with_fdio.h"
#include "heapdump/snapshot.h"

struct LinkedListNode {
  uint64_t value;
  struct LinkedListNode *next;
};

struct LinkedList {
  struct LinkedListNode *head;
};

uint64_t fibonacci(uint64_t n, struct LinkedList *list) {
  if (n <= 1) {
    return n;
  }

  uint64_t result = fibonacci(n - 1, list) + fibonacci(n - 2, list);

  struct LinkedListNode *node = malloc(sizeof(struct LinkedListNode));
  node->value = result;
  node->next = list->head;
  list->head = node;

  return result;
}

int main(int argc, char **argv) {
  heapdump_bind_with_fdio();

  struct LinkedList list = {.head = NULL};

  // Do some tasks that allocate memory at each step.
  for (int i = 0; i < 8; i++) {
    // Take a named snapshot. You can run:
    // - "ffx profile heapdump list" to show the list of all the taken named snapshots
    // - "ffx profile heapdump download" to export one
    char namebuf[20];
    sprintf(namebuf, "fib-%d", i);
    heapdump_take_named_snapshot(namebuf);

    fprintf(stderr, "Iteration #%d...\n", i);
    fibonacci(i, &list);
    sleep(1);
  }

  // Keep this process alive so that it stays possible to "ffx profile heapdump snapshot" it.
  fprintf(stderr, "Done!\n");
  pause();

  return 0;
}

# Role Manager

The Role Manager is a service that allows clients to
[set profiles](https://fuchsia.dev/reference/syscalls/object_set_profile?hl=en)
on threads and vmars.

## Test Details

The tests in the `tests` directory use a test realm
[factory](https://fuchsia.dev/fuchsia-src/development/testing/components/test_realm_factory?hl=en)
to spawn a `RoleManager` instance inside the test realm and then send requests
to it. This test realm factory can be found in the `testing/realm-factory`
directory.

Here's what the test realm looks like:

                                    -----------------
                                    |   Test Root   |
                                    -----------------
                                    /                \
                                   /                  \
                        -----------------          -----------------
                        |   Test Binary |          |   Test Realm  |
                        -----------------          |     Factory   |
                                                   -----------------
                                                           |
                                                           |
                                                   -----------------
                                                   |  Role Manager |
                                                   -----------------

The `Test Root` routes `fuchsia.scheduler.RoleManager` requests from its
`Test Binary` child to the `Role Manager` child (via the `Test Realm Factory`).

One other notable piece of this testing setup is the profile configuration
files. These can be found in the `tests/testing/realm-factory/config` directory
and are built into the `Test Realm Factory` package as `/pkg/profiles`. This
directory is then routed to the `Role Manager` component as `/config/profiles`.

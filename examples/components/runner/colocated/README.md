# Colocated runner example

This is an example of how to implement a component runner where the components
being run are colocated inside the runner process, i.e. these components share
an address space.

## What does this runner do

The `colocated` runner demonstrates how to attribute memory to each component
it runs. A program run by the `colocated` runner will allocate and map a VMO of
a user-specified size, and then fill it with randomized bytes, to cause the
pages to be physically allocated.

If the program is started with a `PA_USER0` numbered handle, it will signal the
`USER_0` signal on the peer handle once it has done filling the VMO, to indicate
that all the backing pages have been allocated.

## Program schema

The manifest of a component run by this runner would look like the following:

```json5
{
    program: {
        // The `colocated` runner should be registered in the environment.
        runner: "colocated",

        // Size of the VMO in bytes. It will be rounded up to the page size.
        vmo_size: "4096",
    }
}
```

## Building

See [Building Hello World Components](/examples/hello_world/README.md#building).

## Running

To experiment with the `colocated` runner, provide this component
URL to `ffx component run`:

```bash
$ ffx component run /core/ffx-laboratory:colocated-runner-example 'fuchsia-pkg://fuchsia.com/colocated-runner-example#meta/colocated-runner-example.cm'
```

This URL identifies a realm that contains a colocated runner, as well as a
collection for running colocated components using that runner.

To run a colocated component, provide this URL to `ffx component run`:

```bash
$ ffx component run /core/ffx-laboratory:colocated-runner-example/collection:1 'fuchsia-pkg://fuchsia.com/colocated-runner-example#meta/colocated-component-32mb.cm'
```

This will start a component that attempts to use 32 MiB of memory, living inside
the address space of the `colocated` runner.

You may replace `32mb` with `64mb` or `128mb` to test different sizes of memory
usage.

You may replace `collection:1` with `collection:2` etc. to start multiple
colocated components in this collection.

Stopping a colocated component will free up the associated memory.


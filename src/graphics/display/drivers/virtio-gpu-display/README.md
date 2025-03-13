# virtio-gpu guest Display Driver

This is a display driver for [the `virtio-gpu` device][virtio-spec-gpu-section]
described in the [virtio specification][virtio-spec].

## Display / GPU split

Conceptually, the `virtio-gpu` device is a combination of a display engine and a
GPU.

The `virtio_gpu_ctrl_type` enumeration in the
[Device Operation: Request header section][virtio-spec-gpu-request-section]
lists all the commands implemented by the `virtio-gpu` device. Conceptually, the
2D commands and the cursor commands map to the display engine, while the 3D
commands map to the GPU.

The display guest driver (this driver) binds to the `virtio-gpu` device, and
mediates the GPU guest driver's access to the `virtio-gpu` device.

## Manual testing

We do not currently have automated integration tests. Behavior changes in this
driver must be validated using this manual test.

1. Launch a QEMU-based emulator.

    ```posix-terminal
    ffx emu start --engine qemu
    ```

2. Launch the `squares` demo in the `display-tool` test utility.

    ```posix-terminal
    ffx target ssh display-tool squares
    ```

3. Add the following footer to your CL description, to document having performed
   the test.

   ```
   Test: ffx target ssh display-tool squares
   ```

These instructions will work with a `workbench_eng.x64` build that includes the
`//src/graphics/display:tools` GN target. The `//src/graphics/display:tests`
target is also recommended, as it builds the automated unit tests.

```posix-terminal
fx set --auto-dir workbench_eng.x64 --with //src/graphics/display:tools \
    --with //src/graphics/display:tests
```

[virtio-spec]: https://docs.oasis-open.org/virtio/virtio/v1.3/virtio-v1.3.html
[virtio-spec-gpu-section]: https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-3960007
[virtio-spec-gpu-request-header]: https://docs.oasis-open.org/virtio/virtio/v1.3/csd01/virtio-v1.3-csd01.html#x1-4110007

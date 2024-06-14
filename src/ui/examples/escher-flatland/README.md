# escher-flatland

This is a minimal example to demonstrate initializing Escher in a component which uses Flatland to
display itself.  The use of Flatland and sysmem are encapsulated within the Vulkan swapchain, which
is provided by the `VK_LAYER_FUCHSIA_imagepipe_swapchain` layer.

To launch:
```
ffx session add fuchsia-pkg://fuchsia.com/escher-flatland#meta/escher-flatland.cm
```

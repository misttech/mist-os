# Overview

`tiling_wm` is a simple window management component for use with in-tree product sessions such as `workbench_session`.

## Usage

### Build with `tiling_wm`

Make sure `//src/ui/bin/tiling_wm` to your gn build args, e.g:

```
fx set <product>.<board> --with //src/ui/bin/tiling_wm
```

### Launch `tiling_wm` via `workbench_session`

```
ffx session launch fuchsia-pkg://fuchsia.com/workbench_session#meta/workbench_session.cm
```

### Add your view

For example:

```
ffx session add fuchsia-pkg://fuchsia.com/flatland-examples#meta/simplest-app-flatland-session.cm
```

## Current limitations

tiling_wm currently supports displaying one view at a time, and launching views for display
over the currently displayed view. In the future, support should be added to display additional views in a tiling, mosaic affect.

# Use cases

`tiling_wm` fills a few roles in the Fuchsia ecosystem:

- educational: explain workings of a simple yet fully-functional window manager
- testing: reliable basis for integration tests
- development: support development by UI teams:
  - inspect
  - expose APIs for tests to query various conditions

# `tiles-flatland` service

`tiles-flatland` is a component which serves the `fuchsia.developer.tiles` protocol.

## Limitations
[https://fxbug.dev/80883](https://fxbug.dev/80883): `tiles-flatland` connects directly to the display, not RootPresenter.  Therefore:
- `tiles-flatland` cannot be running at the same time as RootPresenter
- embedded tile apps will not receive input

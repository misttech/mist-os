# base-resolver

`base-resolver` is a V2 component that:
  1. implements the Package Resolver FIDL protocol [`fuchsia.pkg.PackageResolver`]
     (though it does not implement the `GetHash` method. `GetHash` is used to determine which
     version of a package the ephemeral resolver (`pkg-resolver`) would resolve. The
     `base-resolver` always resolves the version from base, so clients should never need to use
     this method.)
  2. implements the Component Resolver FIDL protocol [`fuchsia.component.resolution.Resolver`]
     and exposes this protocol as a resolver capability.

The responsibility of `base-resolver` is to resolve URLs to packages that are located
in blobfs and are part of the "base" set of packages.

It does this by:
1. Using fuchsia.boot/Arguments to determine the hash of the system_image package.
2. Reading the static packages manifest of the system_image package to discover the mapping
   from base package URLs to hashes.
3. Using an RX* handle to blobfs and the package-directory library to serve the package directories
   directly out of blobfs.

## Building

The `base-resolver` component should be part of the core product configuration and be
buildable by any `fx set` invocation.

## Running

This component is not packaged in the traditional way. Instead, its binary and manifest are
included in the Zircon Boot Image (ZBI) and are accessible via bootfs.

To launch this component, include it as a child in the component topology using the URL
`fuchsia-boot:///#meta/base-resolver.cm`, and include its exposed resolver capability
in an environment.

```json5
{% includecode gerrit_repo="fuchsia/fuchsia" gerrit_path="src/sys/base-resolver/tests/meta/integration-test.cml" region_tag="environment"}
```

## Testing

Unit tests for base-resolver are available in the `base-resolver-unittests`
package.

```
$ fx test base-resolver-unittests
```

Integration tests for base-resolver are available in the `base-resolver-tests` package
(which uses base-resolver as a component resolver) and the `base-resolver-integration-tests`
package, which uses RealmBuilder to fake the dependencies.

```
$ fx test base-resolver-tests base-resolver-integration-tests
```

[`fuchsia.pkg.PackageResolver`]: ../../../sdk/fidl/fuchsia.pkg/resolver.fidl
[`fuchsia.component.resolution.Resolver`]: ../../../sdk/fidl/fuchsia.component.resolution/resolver.fidl

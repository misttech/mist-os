# Use developer overrides for assembly of a Fuchsia product

In some cases, you may want to override the configuration of a Fuchsia product.

## Use assembly overrides {:#use-assembly-overrides}

To start using developer overrides for assembly:

1. In your Fuchsia checkout, make a `//local/BUILD.gn` file:

   A `//local.BUILD.gn` file may look like:

   Note: For the fully list of values that you can specify, see
   [Product configuration][product-config-ref]. Additionally, you can use
   [`developer_only_options`](#use-developer_only_options) to enable certain
   options including board configurations.

    ```gn
    import("//build/assembly/developer_overrides.gni")

    assembly_developer_overrides("my_overrides") {
        kernel = {
            command_line_args = ["foo"]
        }
    }
    ```

   This is what this `BUILD.gn` file indicates:

   * The assembly developer override is named `my_overrides`. You can use
     any meaningful identifier for this field.
   * The keys `kernel`, `command_line_args` are all based on supported values
     for Platform configuration. For more information, see
     [`PlatformConfig`][platform-assembly-ref].

1. Once you have made a `//local/BUILD.gn` file that meets your requirements,
   you can set your `fx set` command to use this assembly platform
   configuration. For example:

   Note: The value used for `--asembly-override` is the identifier that you used
   in your `//local/BUILD.gn` file.

   ```posix-terminal
   fx set --assembly-override=//local:my_overrides
   ```

1. Build Fuchsia:

   ```posix-terminal
   fx build
   ```

   You should now be able to build Fuchsia as you normally would.

Additionally, you can also do the following:

* [Add a package](#add-package)
* [Add a shell command](#add-shell-command)
* [Use `developer_only_options`](#use-developer_only_options)

## Add a package {#add-package}

There may be some cases where you may need to include additional packages to
your product's packages. In this case, your `//local/BUILD.gn` may look
like:

```gn
import("//build/assembly/developer_overrides.gni")

assembly_developer_overrides("my_custom_base_packages") {
  base_packages = [
    "//some/gn/target/for/a:package",
    "//some/other/target/for/a:package",
    "//third_party/sbase",
  ]
}
```

## Add a shell command {#add-shell-command}

There may be some cases where you may need to include shell commands to your
product's configuration. When adding CLI tools for running in the Fuchsia shell,
it's necessary to both add the package and configure assembly to create the
launcher stub that runs the component for the binary. This is due that most CLI
tools are actually components.

To do this, define a `shell_commands` list within your
`assembly_developer_overrides`. Each entry in this list is an object with the
following keys:

* `package_name`: The name of the package that contains the shell command. This
  must match the `package_name` defined in the package's `BUILD.gn` file.
* `components`: A list of the CLI binaries within the package that you want to
  register as shell commands. Assembly automatically adds the `meta/` prefix and
  `.cm` suffix to these names when looking for their component manifests.
  For example, `foo` becomes `meta/foo.cm`.

For example, to add the `cp` command from the `//third_party/sbase` package,
your `//local/BUILD.gn` may look like:

```gn
import("//build/assembly/developer_overrides.gni")

assembly_developer_overrides("my_custom_shell_commands") {
  shell_commands = [
    {
      package_name = "//third_party/sbase"
      components = [ "cp" ]
    }
  ]
}
```

### Add a shell command to a package set {#add-shell-command-package-set}

Additionally, if making the package available through package discovery
isn't sufficient, you can also add the package target to a package set.
For example, to add the package to the `base` package set, your
`//local/BUILD.gn` may look like:

```gn
import("//build/assembly/developer_overrides.gni")

assembly_developer_overrides("my_custom_shell_commands") {
  shell_commands = [
    {
      package_name = "//third_party/sbase"
      components = [ "cp" ]
    }
  ]

  # This GN target should define a package named "foo_cli".
  base_packages = [
    "//some/gn/target/for/my/package:foo_cli"
  ]
}
```

## Use `developer_only_options` {#use-developer_only_options}

In some cases, you may want to use some of the `developer_only_options`. These
options can be combined with the general overrides covered in
[Use assembly overrides](#use-assembly-overrides).

Note: For a full list of supported `developer_only_options` see
[`developer_overrides.gni`].

In this case, your `//local/BUILD.gn` may look like:

```gn
import("//build/assembly/developer_overrides.gni")

assembly_developer_overrides("my_overrides") {
  developer_only_options = {
    all_packages_in_base = true
    netboot_mode = true
  }
}
```

### `all_packages_in_base`

This option redirects all cache and on_demand package-set packages into the base
package set. This feature allows the use of a product image that has cache or
universe packages in a context where networking may be unavailable or a package
server cannot be run.

The primary use case for this option is to allow debugging tools to be available
on-device when networking is non-functional.

### `netboot_mode`

This option creates a ZBI (Zircon Boot Image) that includes the fvm/fxfs image
inside a ramdisk, to allow the netbooting of the product.

This is a replacement for the `netboot` assembly that was previously generated
with `//build/images/fuchsia:netboot`.

[platform-assembly-ref]: /reference/assembly/PlatformConfig/index.md
[product-config-ref]: /reference/assembly/index.md
[`developer_overrides.gni`]: https://source.corp.google.com/h/fuchsia/fuchsia/+/main:build/assembly/developer_overrides.gni;l=13-130
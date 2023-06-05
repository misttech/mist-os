# GN templates for building and validating Device Tree

GN templates and scripts for compiling device tree source files and validating against golden files.

## Examples
* Define a "devicetree_fragment" to process dtsi files.
  ```
  import("//build/devicetree/devicetree.gni")

  devicetree_fragment("chipset-x") {
    sources = [ "dts/chipset-x.dtsi" ]
  }
  ```

* Use "devicetree" template to compile a device tree source file for a board. This additionally compares the resulting dts file with a golden.
  ```
  import("//build/devicetree/devicetree.gni")

  devicetree("board-x") {
    sources = [ "dts/board-x.dts" ]
    # Dependencies for fragments referenced in board-x.dts
    deps = [ ":chipset-x" ]
    golden = "dts/board-x.golden.dts"
  }
  ```
  The output dtb is available by default at `get_target_outputs(":board-x_dtb")` which is equivalent
  to `$target_out_dir/board-x.dtb`. The output path can also be specified by defining `outputs`
  variable during invocation.

* Use "dtb" to compile a `.dts` file into `.dtb`.
  ```
  dtb("test-dtb") {
    sources = [ "test.dts" ]
  }
  ```
  The output dtb is available by default at `get_target_outputs(":test-dtb")` which is equivalent to
  `$target_out_dir/test.dtb`. The output path can also be specified by defining `outputs` variable
  during invocation.

* Use "dts" to decompile a `.dtb` file into `.dts`.
  ```
  dts("test-dts") {
    sources = [ "test.dtb" ]
  }
  ```
  The output dts is available by default at `get_target_outputs(":test-dts")` which is equivalent to
  `$target_out_dir/test.dts`. The output path can also be specified by defining `outputs` variable
  during invocation.

See  `devicetree.gni` file for more details.

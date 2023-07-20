# Build workbench

workbench (`workbench_eng`) is an open source reference design for Fuchsia.
workbench is not a consumer-oriented product. workbench is a tool for
developers to explore Fuchsia and experiment with evolving
concepts and features.

workbench does not come with strong security, privacy, or robustness
guarantees. Bugs and rapid changes are expected – to help improve Fuchsia,
please [file bugs and send feedback][report-issue].

## Get started with workbench {#get-started-with-workbench}

To get started with workbench, you need to be familiar with how to get the
Fuchsia source code, build Fuchsia images, and run Fuchsia on a device or
emulator – the instructions in this section are based on the
[Get started with Fuchsia][get-started-with-fuchsia] flow.

workbench is designed to be used with an Intel NUC or the Fuchsia emulator
(FEMU).

*   {Intel NUC}

    To install workbench on an Intel NUC, do the following:

    1.  Complete the [Download the Fuchsia source code][get-fuchsia-source]
        guide.
    2.  As part of [Configure and Build Fuchsia][build-fuchsia], set your build
        configuration to use the following workbench product:

        ```posix-terminal
        fx set workbench_eng.x64 --release
        ```

    3.  Complete the [Install Fuchsia on a NUC][intel-nuc] guide.

*   {FEMU}

    To try workbench on the Fuchsia emulator, do the following:

    1.  Complete the [Download the Fuchsia source code][get-fuchsia-source]
        guide.
    2.  As part of [Configure and Build Fuchsia][build-fuchsia], set your build
        configuration to use the following workbench product:

        ```posix-terminal
        fx set workbench_eng.qemu-x64 --release
        ```

    3.  Complete the [Start the Fuchsia emulator][start-femu] guide.

<!-- Reference links -->

[report-issue]: /docs/contribute/report-issue.md
[get-started-with-fuchsia]: /docs/get-started
[get-fuchsia-source]: /docs/get-started/get_fuchsia_source.md
[build-fuchsia]: /docs/get-started/build_fuchsia.md
[intel-nuc]: /docs/development/hardware/intel_nuc.md
[start-femu]: /docs/get-started/set_up_femu.md

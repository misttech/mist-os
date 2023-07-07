# Record and visualize a trace

This page describes how to record and visualize a trace on a Fuchsia
device with the [Fuchsia tracing system][fuchsia-trace-system].

## Prerequisites

Important: Tracing is only enabled for `core` and `eng` build types in Fuchsia
images. Make sure that the Fuchsia image you use is of ` core` or `eng` build type.
In other words, if you're to build a Fuchsia image from the Fuchsia source checkout
or download a Fuchsia prebuilt image, make sure that the image's build type is
**not** `user` or `userdebug`.

Many existing Fuchsia components are already registered as trace providers, whose
trace data often provide a sufficient overview of the system. For this reason,
if you only need to record a general trace (for instance, to include details
in a bug report), you may proceed to the sections below. However, if you want
to collect additional, customized trace events from a specific component, you need
to complete the following tasks first:

* [Register your component as a trace provider][register-a-trace-provider]
* [Add tracing in your component's code][add-tracing-in-your-code]

## Record a trace {:#record-a-trace}

To record a trace on a Fuchsia device from your host machine,
run the following command:

```posix-terminal
ffx trace start --duration <SECONDS>
```

This command starts a trace with the default settings, capturing
a general overview of the target device.

The trace continues for the specified duration (or until the `ENTER` key
is pressed if a duration is not specified). When the trace is finished, the
trace data is automatically saved to the `trace.fxt` file in the
current directory (which can be changed by specifying the `--output` flag;
for example, `ffx trace start --output <FILE_PATH>`). To visualize the trace
results stored in this file, see the [Visualize a trace](#visualize-a-trace)
section below.

Note: For more details on the `ffx trace` commands and options, see
[Record traces for performance analysis][record-traces].

## Visualize a trace {:#visualize-a-trace}

[Fuchsia trace format][fuchsia-trace-format] (`.fxt`) is Fuchsia's
binary format that directly encodes the original trace data. To
visualize an `.fxt` trace file, you can use the
[Perfetto viewer][perfetto-viewer]{:.external}.

Do the following:

1. Visit the [Perfetto viewer][perfetto-viewer]{:.external}
   site on a web browser.
2. Click **Open trace file** on the navigation bar.
3. Select your `.fxt` file from the host machine.

This viewer also allows you to use SQL to
[query the trace data][perfetto-trace-processor]{:.external}.

## Appendices

The Fuchsia tracing system previously supported various file formats and
different ways to visualize a trace. The sections below describe
workflows related to those now-deprecated trace formats.

### (Deprecated) Visualize a JSON trace {:#visualize-a-json-trace}

A JSON trace is a format for viewing trace data on a Chrome browser.

To convert an `.fxt` trace file into JSON format, run the following
command:

```posix-terminal
fx trace2json < <FXT_FILE> > trace.json
```

Replace `FXT_FILE` with an FXT trace file, for example:

```none {:.devsite-disable-click-to-copy}
$ fx trace2json < trace.fxt > trace.json
```

To visualize a JSON trace, use Chromium's
[Trace-Viewer][chromium-trace-viewer]{:.external}, whichis built
into a [Chrome][chrome]{:.external} browser.

Do the following:

1. Open a new tab on a Chrome browser.
2. Navigate to `chrome://tracing`.
3. Click the **Load** button.
4. Open your JSON trace file.

For more information on Chromium's Trace-Viewer, see
[The Trace Event Profiling Tool][trace-event-profileing-tool]{:.external}.

### (Deprecated) Visualize an HTML trace {:#visualize-an-html-trace}

A HTML trace is a standalone file that includes both the
viewer and trace data.

To produce an HTML trace, you can use the `trace2html` tool
from the [Chromium Catapult Repository][catapult-project]{:.external}
to convert an existing JSON trace file.

From the Catapult repository, run the following command:

```posix-terminal
./tracing/bin/trace2html <JSON_TRACE_FILE>
```

Once an HTML trace file is generated, you can open the file on any
web browser to analyze the trace results.

#### Navigate an HTML trace file

Tip: In the top right corner of the HTML page, click the small **?** icon
to see help information.

For navigating information on an HTML trace file, the following are some
useful keyboard shortcuts:

  * `w` and `s`: Zoom in and zoom out, respectively. The zoom function is
    based on the current position of your mouse.
  * `W` and `S`: Zoom in and zoom out at a larger scale, respectively. The
    zoom function is based on the current position of your mouse.
  * `a` and `d`: Pan left and right, respectively.
  * `A` and `D`: Pan left and right at a larger scale, respectively.

Also, you can deselect specific process rows to remove processes that aren't
important for your current trace. To deselect a specific process row,
click the **x** in the right corner of the process row.

### (Deprecated) Analyze an HTML trace {:#analyze-an-html-trace}

This walkthrough covers workflows of generating an HTML trace file
and analyzing the results on a web browser. The example in this
walkthrough records a trace of a Fuchsia system while constantly
running the `du` command, which scans and generates the disk usage
of the system.

#### Generate trace data and convert it to HTML {:#generate-trace-data-and-convert-it-to-html}

To record a trace of `du` and convert it to an HTML trace file,
do the following:

1. (**Optional**) If you don't have a running Fuchsia device, start
   an instance on the Fuchsia emulator with networking enabled:

   ```posix-terminal
   ffx emu --net tap
   ```

1. In a new terminal, start a trace:

   ```posix-terminal
   ffx trace start --buffer-size 64 --categories all
   ```

   This command sets a recording buffer size of 64 megabytes and
   records all tracing categories.

1. In the Fuchsia emulator's console, run the following command:

   ```sh
   /boot/bin/sh -c "'\
      sleep 2 ;\
      i=0 ;\
      while [ \$i -lt 10 ] ;\
      do /bin/du /boot ;\
          i=\$(( \$i + 1 )) ;\
      done'"
   ```

   This command runs `du` in a loop,

   Note: For more information on creating a process in Fuchsia, see
   [Process creation][process-creation].

1. To finish the tracing, press `Enter` key in the terminal
   where `ffx trace start` is running.

   When finished, the command generates a `trace.fxt` file.

1. Convert this FXT file to JSON format:

   ```posix-terminal
   fx trace2json < trace.fxt > trace.json
   ```

1. Generate an HTML trace:

   ```posix-terminal
   ./tracing/bin/trace2html trace.json
   ```

1. Open this HTML file on a web browser.

   ![Screenshot of trace interface](images/trace-example-overview.png "Image showing the trace interface on Chrome"){: width="600"}

   **Figure 1**. Screenshot of an HTML trace file that recorded a `du` process in a
   loop.

   A trace file has a lot of information including a time scale near the top of
   the trace. In the example above, the whole trace lasted about 2.5 seconds.

#### Examine CPU usage {:#examine-cpu-usage}

The region marked by the yellow circle in Figure 1 shows the CPU usage area
where you can see the overall CPU usage on all CPU cores.

#### Examine program execution {:#examine-program-execution}

The region marked by the green circle in Figure 1 shows the program execution.

In this example, you can see 10 invocations of the `du` program, which is
expected since the trace was recorded during a loop of `du`. Therefore,
you can see 10 different `du` process IDs, one after the other.

#### Examine blobfs CPU usage {:#examine-blobfs-cpu-usage}

The region marked by the blue circle in Figure 1 shows the CPU usage to write
to the blobstore filesystem (blobFS).

In this example, you can see little bursts of CPU time that are each related
to an invocation of `du`. At this high level, it can be difficult to determine
the exact correlation between the CPU usage and the filesystem, for instance,
for the following reasons:

* Is the CPU usage caused by the loading of `du` from the filesystem?
* Is the CPU usage caused by the execution `du` as it runs through the target
  filesystem to see how much space is in use?

You can zoom in on specific areas of this region to determine the correlation
between the CPU usage and the filesystem (see Figure 2).

![Screenshot of zooming in on cpu and filesystem information](images/trace-example-zoom1.png "Image showing CPU and filesystem trace information"){: width="600"}

**Figure 2**. Screenshot of the HTML trace file zoomed into a region.

In this example, you can see just two `du` executions (the first is marked
with a green circle). The first `blobfs` CPU burst actually consists of
three main clusters and some smaller spikes. Subsequent `blobfs` CPU bursts
have two clusters.

From analyzing this example, you can see that the `blobfs` bursts happen
before the `du` program is executed. This information shows that the
`blobfs` bursts are not due to the `du` program reading the filesystem.
Instead, it shows that the bursts are due to loading the `du` program.

You are now ready to dive further into what is causing the `blobs` bursts.

![Image of blobs trace timings](images/trace-example-blobfs1.png "Image showing blobs trace information"){: width="600"}

**Figure 3**. Screenshot of the HTML trace file showing a time scale of about
1 millisecond.

In this example, notice the time scale that spans a time period from 2,023,500
microseconds to just past 2,024,500 which indicated a time scale of about
1 millisecond. During that millisecond, `blobfs` executed code,
starting with a process identified as `FileReadAt`, which then called
`Blob::Read`, which then called `Blob::ReadInternal`.

To correlate this information with the code, you can click on parts
of the report for more detailed information about a specific object:

* If you click on `FileReadAt`, you may see information similar to
  the following:

  ![Image of FileReadAt information](images/trace-example-filereadat.png "Image showing FileReadAt trace information"){: width="300" border="1"}

  This information tells you the following:

  *  The trace category for `FileReadAt` is `vfs`.
  *  The length of time of the function execution.

  To understand how tracing is performed for `FileReatAt`,
  see [`//src/lib/storage/vfs/cpp/connection.cc`][connection-cc].

* If you click on `Blob::Read`, you may see information similar to
  the following:

  ![Image of Blob::Read information](images/trace-example-blobread.png "Image showing Blob::Read trace information"){: width="300"}

  Below is the code for `Blob::Read`:

  ```cpp {:.devsite-disable-click-to-copy}
  zx_status_t Blob::Read(void* data,
                         size_t len,
                         size_t off,
                         size_t* out_actual) {
      TRACE_DURATION("blobfs", "Blob::Read", "len", len, "off", off);
      LatencyEvent event(&blobfs_->GetMutableVnodeMetrics()->read,
                         blobfs_->CollectingMetrics());

      return ReadInternal(data, len, off, out_actual);
  }
  ```

  This code calls calls the `TRACE_DURATION()` macro with the category of `blobfs`,
  a name of `Blob::Read`, and a length and offset key and value pairs. All
  of this information is recorded in the trace file.

  To understand how tracing is performed for `Blob::Read`,
  see [`//src/storage/blobfs/blob.cc`][blob-cc].

<!-- Reference links -->

[fuchsia-trace-system]: /docs/concepts/kernel/tracing-system.md
[register-a-trace-provider]: /docs/development/tracing/tutorial/register-a-trace-provider.md
[add-tracing-in-your-code]: /docs/development/tracing/tutorial/add-tracing-in-code.md
[record-traces]: /docs/development/sdk/ffx/record-traces.md
[fuchsia-trace-format]: /docs/reference/tracing/trace-format.md
[perfetto-viewer]: https://ui.perfetto.dev
[perfetto-trace-processor]: https://www.perfetto.dev/#/trace-processor.md
[chromium-trace-viewer]: https://github.com/catapult-project/catapult/tree/HEAD/tracing
[chrome]: https://google.com/chrome
[trace-event-profileing-tool]: https://www.chromium.org/developers/how-tos/trace-event-profiling-tool
[catapult-project]: https://github.com/catapult-project
[process-creation]: /docs/concepts/process/process_creation.md
[connection-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:/src/lib/storage/vfs/cpp/connection.cc
[blob-cc]: https://cs.opensource.google/fuchsia/fuchsia/+/main:/src/storage/blobfs/blob.cc

# Tracing affordance

[TOC]

This page talks about [Tracing] affordance in Honeydew.

## Usage
```python
# For single trace, use the tracing context manager to capture trace.
>>> with emu.tracing.trace_session():
...     # Execute workload
...
# For single trace with download, use the tracing context manager to capture trace.
>>> with emu.tracing.trace_session(download=True, directory="/tmp/log", trace_file="trace.fxt"):
...     # Execute workload and access the trace at specified path.
...

# To perform multi trace within the same session, call the methods directly
>>> emu.tracing.initialize()
# First trace
>>> emu.tracing.start()
>>> emu.tracing.stop()
# Second trace
>>> emu.tracing.start()
>>> emu.tracing.stop()
# call terminate when we want to terminate the trace session without saving the trace.
>>> emu.tracing.terminate()
# call terminate_and_download when we want to terminate the trace session and save the trace.
>>> emu.tracing.terminate_and_download(directory="/tmp/")
```

[Tracing]: ../interfaces/affordances/tracing.py

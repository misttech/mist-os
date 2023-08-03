# Fuchsia syslog structured backend

The Fuchsia syslog structured backend allows you to write structured logs to
Archivist. Structured logging allows you to encode more information in your log
than just text, allowing for richer analytics and diagnostics of your component.
This library is meant for systems integrators, not application developers.
https://fxbug.dev/81491 tracks app development support.

## Usage

Before using the API, you need to connect to Archivist using the LogSink
protocol. After connecting, invoke ConnectStructured on the LogSink and pass in
a socket. Use the other side of the socket to do any of the following:

### Encoding a regular key-value-pair message

```cpp
fuchsia_syslog::LogBuffer buffer;
buffer.BeginRecord(severity, file_name, line, msg, condition, logsink_socket, number_of_dropped_messages, pid, tid);
number_of_dropped_message+=buffer.FlushRecord() ? 0 : 1;
```

NOTE: Logs are best-effort (as are all diagnostics), a return value of true from
FlushRecord means that the log was made available to the platform successfully,
but it is not a guarantee that the message will make it to the readable log (for
many reasons, including budget constraints, platform issues, or perhaps we're
running on a build without logging at all).

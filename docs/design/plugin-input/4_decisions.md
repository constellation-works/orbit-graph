# Plugin request byte bound

The executable reads at most 1 MiB plus one sentinel byte from plugin stdin.
The limit covers the complete envelope, including context and plugin config.
An oversized request is refused before JSON decoding and dispatch with an
`invalid_request` envelope, an actionable byte limit, and exit zero as required
by the exec protocol. A request exactly at the limit remains accepted.

Plugin inputs contain selectors, paths, revisions and bounded query parameters;
source code and graph payloads are read from the routed repository rather than
sent in the request. This bound prevents a peer from growing the input buffer
without limit (STD-03 §R22). Orbit owns the call timeout and terminates a backend
whose stdin stalls; the executable does not create another timer or worker.

The real executable regression sends valid version requests padded with JSON
whitespace. The oversized case returned success before the fix; it now refuses
the request without creating state. The boundary case verifies that the bound
does not truncate a valid request (STD-04 §R1, STD-04 §R2).

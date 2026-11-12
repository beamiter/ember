# Projection availability and allocation budgets

A requested document view is now rejected before expansion if bounded metadata
cannot prove it fits the host policy. Rejection is an explicit unavailable empty
frame, not an identity/raw/live-grid fallback: scoped or Chat content must not
leak through a different presentation. The process continues; keyboard input,
resize and closing remain available. Pointer, selection/copy, search navigation,
links and file drops must not use unavailable geometry. The renderer explains
that content is not being displayed and the terminal is still running.

Limits: 101024 source rows, 131072 planned rows, 262144 raw slices and 1048576
wide-continuation scratch entries. Raw materialization separately caps 1048576
actual decoded cells, including visible output. Ember does not clone compressed
history here, so it has no compressed-clone budget or unused allocation-failure
reason. The raw logical-group estimate counts actual source columns, not trimmed content.

Metadata-only planning does not apply the raw-cell limit to the full history.
It computes checked conservative upper bounds without collecting source layouts:
wide-aware rows use at least `cols-1` source cells except the last row. This may
reject some documents below the nominal row limit, especially at width two.
The slice estimate includes source boundaries and collapse endpoints; resolved
collapse intervals are disjoint by the existing overlap rejection rule. Chrome,
Chat and incremental-before-eviction growth are included before they allocate.
Previously implicit source-plus-output scratch has independent caps.

Transformed materialization keeps only one decoded historical row rather than
retaining every source row intersecting the viewport. Its output plus Chat dock
is separately bounded before materialization. Existing bounded Vec/Arc
allocations may still fail under process-wide memory pressure: this does not
claim to intercept every allocator OOM or to impose a CPU time deadline.

Successful and rejected UI assessments are cached by scalar source revisions,
dimensions, scroll offset and buffer identity. Formal policy/scope/Chat changes
invalidate this cache. Repeated idle guards do not rescan history or construct
policy keys; planning also caches its assessment using the exact plan key.
Rejection drops old plans, viewport caches and selections. Frame revocation is
shared across previously returned frames; recovery creates a new token so old
frames never regain live-action authority. Immutable snapshot inspection
(cells, recorded origins, row kinds and stable anchors) remains available for
cache reuse and anchor restoration. It is not authorization: is_available,
application_cell and host/UI live-action entry points enforce current tokens. The formal projection entry point retries
when source, width or policy changes. An ancillary raw read cannot clear a
formal rejection. Headless tests are not native GUI acceptance.

Raw admission is also cached by scalar source/offset/limits. Materialization
rejection preserves numeric scroll/follow intent while discarding stale anchor
authority; its cache includes the original request, effective mode and margin.
An unchanged unavailable request does not silently jump to the live bottom.

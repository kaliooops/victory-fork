# Comparing culling inputs

`cap-a`, `cap-b`, and `cap-c` contain binding descriptions and clamped
indirect arguments, but no raw buffer snapshots. Their input bytes cannot
be recovered from those files. Keep them as the original bad/good evidence;
collect new good and bad runs with the following diagnostics.

```powershell
$env:VICTORY_CAPTURE = 'D:\GTA6-Research\victory-data\cull-run-01'
$env:VICTORY_CAPTURE_BUFFERS = '9146291b38ea9ce1,3b471c122be79b3e,9f017d9011377efa'
$env:VICTORY_CAPTURE_BUFFER_HEAP = '17'
.\play.ps1 -Diag
```

At the prison, create `capture-now` in that folder. It records three
frames, as before. Record whether walls are visible, and retain the log.
Use a new folder for every run. Obtain both outcomes across several runs;
one or two successes do not establish a fix.

Alongside the existing picture, ops and args files, each listed frame has
a `frame-N-buffers` directory. Files are named by dispatch sequence,
shader hash, binding, and phase (`before` / `after`). Every binary has a
text sidecar with its title address, size, shadow identity, descriptor
match status, BLAKE3, and zero-byte count. Root constants are separate
little-endian dword binaries. For these passes the relevant bindings are:

| Binding | Meaning |
|---------|---------|
| `Srv-s0-d0` | 288-byte input |
| `Srv-s0-d8` | 456-byte input |
| `Uav-s0-d24` | meshlet output list |
| `Uav-s0-d32` | indirect arguments |
| `heap-17` | geometry heap (verify this index against roots in ops) |

The selected heap is observed only for a shader using indexed buffers.
Large buffers (over 4 MiB) are captured only before their first selected
use in each listed frame; small buffers are captured around every selected
dispatch. GPU readback allocation is limited to 512 MiB per listed frame;
omissions are warned in the log. Descriptor mismatches have a text report
instead of a misleading snapshot from another copy.

`before-title-at-submit` files contain CPU title memory during submission
preparation. They are not GPU input snapshots: earlier GPU work may still
produce or change those bytes. Compare them with the GPU `before` files
with that distinction in mind. Unreadable CPU ranges are omitted and
warned, never silently filled with zeros.

GPU snapshots are copies recorded at the dispatch, saved by the list's
completion path after its fence, including with late waits. They do not
refresh buffers or mirror descriptors. Extra barriers, readbacks, and file
I/O do alter timing; disappearance of the intermittent bug under capture
would not prove it fixed. Release behavior is unchanged.

Match passes using their shader and argument-output title address, then
their order within that pass. Do not match global sequence numbers across
runs: other command lists record concurrently. Start with the small
inputs, then use their heap offsets to restrict comparisons of the large
heap. Keep every binary and derived report outside the repository.

`tools/compare-buffer.ps1 -Good <file.bin> -Bad <file.bin>` streams the
comparison and returns differing byte/dword counts, changed 64 KiB pages,
and the first differing words as hex and floats. `-Offset` and `-Length`
(multiples of four) restrict it to a heap range. Pipe its result to
`ConvertTo-Json -Depth 5` to save a report outside the repository. It can
also compare a GPU input with its `title-at-submit` counterpart; differing
bytes there need interpretation of earlier GPU producers.

Validation: the capture test preserves different bytes before and after a
GPU copy in one list and checks that no file is saved before submission.
The diagnostics GPU suite passes (118 tests, one benchmark ignored), and
diagnostics clippy passes with warnings denied. This is instrumentation,
not a fix for the missing walls.

## First runtime validation (2026-11-25)

Instrumentation and comparison tool are backed up on `origin/main` in
`ba6a4fe` and `5096d28`. A diagnostics run was launched with the settings
above into `victory-data/cull-run-01`. Its first capture is frames
1814-1816, during the early transition/loading sequence, **not a labeled
good/bad prison capture**. In frame 1814, all captured static-cull small
inputs and the complete geometry heap matched the corresponding CPU
title-memory samples exactly. Of 60 before/title comparisons, ten differed,
all in UAV outputs (which earlier GPU passes produce). No descriptor
mismatch files were emitted in that frame. The comparison report is
outside the repository at
`victory-data/cull-analysis/run01-frame1814-title-vs-gpu.json`.

This validates in-game file generation and provides no diagnosis of the
intermittent prison bug. Another `capture-now` in the same running session
can capture the prison once reached. Still needed: new prison captures
with both wall outcomes, matched by output-argument buffer and pass order.
The old captures remain the original evidence but lack raw input bytes.

# Multi-wave compute investigation, 2026-11-25

This is a Phase 1 research checkpoint for the four-phase request. It does
not establish deterministic prison rendering, end-to-end ray tracing, or a
performance improvement. Phases 2-4 have not been implemented here.

## Implemented

- Group-wide LDS voting for explicitly EXEC-masked widened branches.
  Each host wave publishes a vote; the group reads all votes between two
  barriers. Data, SCC and mask exchanges still use each console wave's own
  pair of slots.
- Hardware execution tests for 128- and 256-thread groups, including
  inactive lower or upper console waves, partial half-waves, guarded LDS
  writes, lane-32 reads and masked loops with different exit times.
- A negative CFG test rejects a two-arm diamond rather than treating it
  as an ordinary EXEC skip.
- Descriptor tracking uses the original logical lane paths while the
  generated control flow is widened. A synthetic descriptor regression
  validates this distinction.
- Widening retries more than the first rewritten program form and includes
  `needs_every_lane` failures in the compute retry condition.
- `VICTORY_RESOURCE_WHY=1` explains failed descriptor provenance.
  `VICTORY_TRANSLATE_PROGRESS=1` logs each container hash before translation,
  which retains useful evidence if a native validator or driver crashes.
- Embedded container sources are bounded by their declared size; their
  hashes/dumps no longer include subsequent containers or trailing data.
  The scanner has a synthetic regression test for both cases.
- The existing physical remapping test now waits for its reader's first
  protected read before starting remappings. A heavily loaded test run
  exposed that it could previously finish and stop an unscheduled reader,
  reporting zero reads rather than testing the intended concurrent access.

Multi-console-wave widening is **off by default**, behind
`VICTORY_GROUP_WIDE=1`, and its setting is included in the shader cache key.
It is experimental and has not passed the full Phase 1 runtime gate.

## Concrete offline result

With the experimental switch and host waves restricted to 32,
`CS_AlphTransmissionPrefilterMip` (`2da4beb386509b77`) emits `WaveSize=32`,
passes the Windows SDK DXIL validator and creates a host compute pipeline.
Before retaining logical descriptor provenance, the widened form failed
descriptor tracking. The old form still requested 64 lanes and its host
pipeline was refused.

`CS_DefragHeap_ComputeRelocations` (`02384a60fda6f2d0`) remains refused.
It contains a VCC-controlled two-arm region and masked loops requiring
preservation of each console wave's logical control path. The implementation
does not pretend that a single group vote solves those cases.

## Why the general restriction remains

Keeping EXEC predicates does not, by itself, preserve scalar register
updates or scalar phi values when an extra console wave enters a region.
A VCC/SCC branch may also leave EXEC nonempty when its wave exits a loop.
For example, two console waves can make opposite `VCC == 0` loop decisions.
Reducing the underlying set bits across the whole group gives an ALL of
those decisions, whereas keeping a physical group loop alive needs ANY of
the console waves that continue. Continuing exited waves without retaining
their logical path can perform additional work; exiting the group early
can discard another wave's work.

The next translator work must retain per-console-wave decisions, serialize
both live arms with appropriate effect predicates, select scalar/phi results
on the original wave decision, and freeze exited waves while other waves
continue. It needs GPU tests for these cases before enabling them generally.
The existing EXEC-masked tests do not prove that broader transformation.

## Validation records

All reports and the shader corpus remain outside the repository under
`victory-data/cull-analysis`:

- `phase1-workspace-tests.txt`: earlier complete workspace run passed;
  subsequent tests and final checks are recorded separately.
- `phase1-alph-semantic-resource.txt`: the transmission shader's successful
  validation and pipeline creation with the experimental switch.
- `phase1-defrag-rotated-loops.txt`: the remaining defragmenter refusal.
- `phase1-all-shaders-experimental.txt`: the first directory sweep exited
  with Windows access violation status `0xc0000005`; it is not a pass.
- `shaders-native-corpus.bin`: concatenation of all 2,961 original saved
  containers for batched offline checks, with an external offset/name map.
  This allows workers to reuse their devices instead of constructing a
  device for each saved file. It is not a repository fixture.
- `phase1-corpus-experimental.txt`,
  `phase1-corpus-default-validation.txt`,
  `phase1-final-workspace-tests.txt`: final check logs; consult their actual
  summaries rather than treating process exit zero as shader success.

The default-path validation sweep completed on all 2,961 native programs.
Its per-program translation/validation outcomes matched the earlier build
exactly (zero outcome differences); existing emit and validation failures
remain. The comparison is
`phase1-baseline-outcome-diff.json`. Container hashes in these pre-scanner-fix
bundled reports include trailing bytes, so use the external offset/name map
or the DXIL code hash for correlation. Newly built tools fix that identity
problem; the decoded programs in the earlier comparison are unchanged.

The exploratory pipeline sweep also completed without an access violation
when the workers reused devices: 2,961 programs, 2,953 emitted shaders,
2,935 validator successes, and 2,952 host pipeline creations. Its sole
pipeline refusal was the defragmenter. The eight emission failures and
existing validation failures remain. This sweep predates the final,
stricter refusal of VCC/SCC group widening and is evidence about that
exploratory revision, not a guarantee for unsupported logical wave paths.

Concurrent native GPU checks also encountered an access violation in one
workspace run. Do not treat that run as successful or infer its cause from
timing alone. The final standalone workspace run is recorded in
`phase1-serial-workspace-tests.txt`, with other GPU jobs stopped.

Final standalone `cargo test --workspace --release --locked`: **502 passed,
0 failed, 7 ignored**. The final transmission/defragmenter recheck is
`phase1-final-targets.txt`: both DXIL programs validate, the transmission
program creates a 32-lane pipeline, and the defragmenter remains refused
under the conservative logical-wave-path guard. The build remains on the
backup branch `wip/group-wide-compute`; no game launch or promotion of the
experimental switch follows from these partial results.

No game was launched for these Phase 1 changes because the requested
offline gate is not yet met. The bad prison run captured before this
request is `victory-data/cull-run-01`, user-confirmed walls missing, with
raw buffer captures at frames 2228-2230 and 2292-2294. Its first cull heap
differs from CPU title-memory samples by roughly 0.65-0.69 MB in these
frames. Earlier GPU producers and submission ordering still need to be
accounted for before those differences can be called blind writes or
attributed to the missing defragmenter.

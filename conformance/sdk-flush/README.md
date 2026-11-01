# Supplemental matching-release persistence oracle

Run `node conformance/sdk-flush/probe.cjs`. This evaluates the byte-exact
JsonlBackendTracker class from the published JSONL backend 0.2.1-alpha.2, plus
the actual SessionHandleClosedError class from the prototype-locked abstract
persistence package. All imports outside those bodies are test-owned ports;
handles are synthetic, not a full SDK filesystem/Agent integration.

The prototype lock includes the abstract service, not this backend package.
This is explicitly supplemental matching-release evidence, not a silent change
to reference commit 6337fc823ce20251c8de8085948165c81aaa701e or its 216 files.
The downloaded tarball SHA-512 integrity was checked against registry metadata;
source hashes and exact URL are recorded. No npm lifecycle scripts, dependency
installation or production routing were run/changed. DeepSeek MIT license is
retained alongside the unmodified distributed JavaScript sources.

Six probe groups cover eager snapshot, reservation/adoption order, sequential
blocking, late registration exclusion, original ordered AggregateError identities
(including a single failure), continuation, actual closed-error handling, reopen
order and handle replacement after an in-flight snapshot.

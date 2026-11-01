# Prepared input persistence observation

`PendingInputAdmission::finish` retains its existing full actor operation
contract. The additive `finish_persistence` releases the same producer boundary
and waits only for the actual prepared journal flush and minimum revision. It
never certifies Core delivery, model success, authority or current object life.
Legacy `begin_input_admission` tickets explicitly reject the new observation;
rejection still releases their owning actor and does not retract accepted input.

## Sole owner and ordering

No second writer, Inbox, Agent loop or execution/control lease is introduced.
The sole worker polls its existing owned flush/control futures. Persistence is
published inside the successful flush future before FIFO actor publication, so
an earlier unresponsive Core control acknowledgement cannot hold later fsync
certificates hostage. Normal InboxInserted/wake/control publication and exact
source-run/origin checks remain ordered and unchanged. No execution precedes
its required fsync.

## Error ownership

Prepared operations have two observation ports but one failure ownership state.
The unused port is closed; dropping it does not report while another observer
can still receive the original typed result. A returned original error marks
that failure observed. All receivers abandoned, or a later control failure after
a successful persistence-only acknowledgement, records once at the actor owner
boundary. A generic channel closure does not pretend to observe a concrete
fsync failure buffered on the other port. Pre-commit preparation cancellation is
not an accepted-input failure; commitment racing owner abandonment is accounted
for in both orders. State contains only observation metadata, not another input
queue, journal, transcript, timer or control task. Public typed errors are cloned
losslessly; there is no stringification or generic Shared error substitution.

## Evidence and remaining scope

An actual Core task is blocked synchronously in a test provider, on a four-thread
runtime with an RAII release guard. Three prepared modes persist and acknowledge
while that Core cannot poll commands; a full legacy observer remains pending
after real flush until the Core is released. Ownership tests exercise buffered
errors, cancellation, later control errors, owner abort and concurrent producer /
actor abandonment. Existing full observers, legacy physical stores, auto gate,
origin fences and held-prefix cases remain required regressions.

This API alone is not full SuperAgent/SDK parity. Native composition must opt in
through controlled Runtime sync and be tested with its real callbacks and Core;
all-journal flush registration order, permanent synchronous facade, full phases,
root/subscription/authorization/tools/Source/API/UI/deploy acceptance remain open.


## Synchronous release prefix

`PendingInputAdmission::begin_persistence()` is a regular function: it releases
its producer boundary before returning an unpolled, owned observation future.
This permits the product to invoke its service-wide persistence barrier at the
original post-accepted hot position, rather than waiting for one blocked input
first. `finish_persistence()` remains an async convenience, releasing on first
poll, and old `finish()` still includes the complete actor operation. Only a
successful awaited observation certifies fsync/minimum revision. An unpolled or
dropped observer is not success and never cancels accepted work. Shared failure
ownership, typed errors, control/run fences and all legacy contracts are unchanged.
The actual host integration covers all three modes with unpolled observations
and dropped observations; this API is not a complete SDK global writer registry
or permanent synchronous Agent facade.

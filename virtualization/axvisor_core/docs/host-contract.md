# Control vCPU execution and revocable backing

This contract covers CPU access to 4 KiB private anonymous user memory on VMX,
Arm VHE and RISC-V H. Retained memory remains the default. A host opts into
notified memory by providing `RevocableMemoryOps`. A backend must advertise
`SUPPORTS_NOTIFIED_BACKING`; an unsupported combination fails VM creation.

## G1. Deliver work to the owner of a running vCPU

The vCPU owns its pending software interrupts across preemption and migration.
The control event queue and its published executor CPU use the same lock.
Enqueue before publication prevents entry until the owner drains the queue.
Enqueue after publication takes a snapshot of the CPU and sends an asynchronous
doorbell after unlocking. A delayed doorbell may reach an old CPU; the queue
retains the event. Linux and Asterinas provide a nonblocking IPI callable from
IRQ context. A host implementing the doorbell also supplies a stable logical CPU
ID while pinned. A host without that pair cannot promise prompt guest exit.

A blocked executor consumes the pending predicate through its host wait path.
For Arm, queue insertion also wakes the native wait queue. The x86 KVM HLT
path polls the predicate while yielding so it can also service emulated timers.
A doorbell signals work. Waiting for the host IPI handler, waiting for a
memory access interval to drain, and joining an executor each have different
completion conditions. The synchronous Arm stop and RISC-V IMSIC operations
retain their own acknowledgement paths.

## G2. Retire guest access before the host changes backing

The host subscribes before the first mapping. Invalidate-start closes access,
advances the epoch, kicks executing CPUs, drains short CPU access intervals and
removes every intersecting mapping including aliases. End permits new accesses
once overlapping invalidations finish. Resolution may sleep outside the gate.
Before installing a page the core checks the subscription, epoch, slot and
permissions under its publication lock. Each temporary reference is finished
even when publication loses a race. Release prevents new publications and
unregistration drains callbacks before freeing their context.

On a subsequent guest access the backend must synchronize stale translations
on the executing CPU before reuse. VMX uses EPT invalidation; VHE and RISC-V
also track the VMID and its incarnation. A CPU change can require synchronization
without another MM notification. Guest-initiated remote VS fences have a
separate completion condition on all addressed harts. An active notified
provider excludes VFIO DMA mapping until device translations and in-flight DMA
have their own revocation and completion protocol.

## G3. Bind CPU-local state only while migration is excluded

The control thread can migrate between bindings. From binding through hardware
entry, host-state restoration and unbind, the host must keep the executor on
one CPU and prevent preemption. The core enters this interval under the local
IRQ guard, checks the translation generation and publishes the executor before
running the guest. The backend restores the host context needed for the first
native operation before returning. The core clears the publication even when
entry returns an error, then releases the binding before dropping the guard.
Fault resolution and native waits can sleep only after leaving that interval.

The per-task identity used by the control frontend may span a sleep; the
published CPU and hardware binding cannot. Each new binding obtains its CPU
identity again. Hosts with a scheduler that can migrate a task while local IRQs
are disabled must supply an equivalent pin spanning the entire binding. A
cached CPU number or VMID alone is insufficient evidence of residency.

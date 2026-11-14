# Architecture

DOSH is a supervisor that owns a physical machine and uses it to host DOS
programs. This document describes the pieces, the boundaries between them,
and the decisions behind them. It describes what exists; plans are in
[roadmap.md](roadmap.md).

```
                 ┌──────────────────────── supervisor (crates/supervisor) ───────────────────────┐
  transports     │  present        capsule                                 hv          platform  │
 ┌──────────┐    │ ┌─────────┐   ┌──────────────────────────────────┐   ┌────────┐   ┌─────────┐ │
 │ COM2..4  │◄──►│ │Endpoint │◄─►│ Console device   DOS personality │   │ Vm     │   │ memory  │ │
 └──────────┘    │ └─────────┘   │ (byte queues)    (crates/dos)    │◄─►│ (SVM)  │   │ tables  │ │
 ┌──────────┐    │               │        ▲               ▲         │   │        │   │ clock   │ │
 │ COM1 log │◄───│── log ────────│        └── service gates ──┐     │   └───┬────┘   │ UARTs   │ │
 └──────────┘    │               └────────────────────────────┼─────┘       │        └─────────┘ │
                 └────────────────────────────────────────────┼─────────────┼────────────────────┘
                                                               │   VMRUN / #VMEXIT
                                                      ┌────────┴─────────────┴───────┐
                                                      │ guest: one DOS program,      │
                                                      │ real mode, 1 MiB, native CPU │
                                                      └──────────────────────────────┘
```

## Lifecycle

**Firmware phase** (`boot.rs`). DOSH starts as `\EFI\BOOT\BOOTX64.EFI`. While
boot services exist it does exactly two things: claims a fixed pool of
memory (64 MiB) for everything it will ever allocate, and reads its work
from the EFI system partition it booted from — `\DOSH\BOOT.CFG`, one
`run [limit=SECONDS] PROGRAM [arguments]` line per capsule, and `\DOSH\C\`, the files each
capsule finds on drive C:. A `supervisor stay` line keeps the supervisor up for the operator's commands. A `console mux` line puts every capsule on one
serial line, switched with Ctrl-] and a number (see `present.rs`). `limit=` is a watchdog: a capsule that has used
that much processor time (ticks that found it running, not time spent idle or
waiting for input) is stopped with a fault that says so. It also reads the
firmware's wall clock once, which DOS date and time are kept from. Then it calls `ExitBootServices`. Nothing after
that point depends on the firmware except the final power-off, which uses
the UEFI runtime service that exists for that purpose.

**Takeover** (`platform/`). The supervisor installs its own identity-mapped
page tables, GDT, TSS and IDT, silences the interrupt controllers the
firmware left running, starts its own clock (a local-APIC timer at a nominal
100 Hz), and switches to its own stack.

The clock has two roles, kept distinct. The scheduler needs only a regular
tick, and gets one either way. The guest's timebase needs the tick rate to
be known: the APIC timer is calibrated against the PIT, and the boot log
reports `timer hz=100 calibrated=yes|no`. When calibration fails the
supervisor falls back to an arbitrary period. Guests are still preempted,
but their clock rate is then unknown. Guest interrupts follow the BIOS count,
which is derived from that tick, so their rate is only as good as the
calibration.

**Supervision** (`main.rs`). Hardware virtualization is enabled, one capsule
is built per manifest line, and the scheduler runs them round-robin until
none is live. Then the machine powers off.

With `supervisor stay` in the manifest the supervisor does not power off
when the manifest's capsules are done. It listens on COM1 for the
operator's commands (`mgmt.rs`): list, launch, suspend, resume, kill, show
and store files on the persistent disk, shut down. The replies are log lines
(`mgmt cmd=... status=ok|error`), so one line carries both the command and
the supervisor's account of what happened. Suspending a capsule stops
scheduling it and freezes its clock (the guest does not see the pause).
A capsule that finishes or is killed is *retired*: its guest memory and
drive go back to the pool, and only its endpoint stays, because on a shared
console line the shown capsule's endpoint is what hears Ctrl-].

## Storage

`dosh-fat` (`crates/fat`) is a small FAT16/FAT32 driver for the files in a
root directory, over any `Block` device. `platform/virtio.rs` is the one
block device: a polled virtio-blk driver on the legacy I/O-port interface,
found by scanning PCI. The supervisor attaches the first such device whose
FAT volume is labelled `DOSHDATA` (`storage.rs`) and leaves every other
disk alone, so the firmware's boot disk is never touched.

The disk is the persistent half of drive C:. A capsule launched at any time
finds the ESP's `\DOSH\C\` files with the disk's files laid over them. When
a program exits (not when it faults or is killed) the files it created or
changed are written to the disk and the ones it deleted are removed from
it, and the supervisor logs each (`storage event=write ...`). The disk is
synchronous: when the log line appears the data has been handed to the
device. Capsule files are shared with what stocked them (the ESP seed and a
snapshot of the disk, rebuilt when the disk changes) and copied only when a
program first writes one. Two capsules that change the same file do not
race: the first to exit lands, and the other's change is refused and
logged, since it started from contents that are gone. A program that deletes
a base-drive file leaves a whiteout on the disk (`WHITEOUT.DSH`, kept by
`storage.rs`, hidden from capsules) so the seed, which is never written, does
not bring it back. A capsule's room to grow is capped by the disk's free
space.

Legacy virtio rather than modern because the legacy registers are in an
I/O BAR, in the first 64 KiB of port space, whereas a modern device's are
in a memory BAR the firmware may put above anything the supervisor maps.
Real hardware will want AHCI or NVMe, or a wider identity map; neither is
needed to prove persistence. The disk image is also an ordinary FAT image:
`mdir`, `mcopy` and `mtype` read and write it while no supervisor has it.

## Reaching a running supervisor

`tools/doshlab.py serve` boots a supervisor that stays up and puts two
serial lines on TCP ports: COM1 (log and management) and COM2 (every
capsule's console, shared and switched with Ctrl-] and a digit). `socat`
from anywhere that can reach those ports attaches. The wire is QEMU's: the
supervisor has no network driver of its own (see the roadmap). The lines are
unauthenticated, so they listen on loopback unless told otherwise.

The supervisor log on COM1 records each stage as a line of `key=value`
pairs. Booting, enabling virtualization, and a guest actually running are
separate events, because they are separate facts: on a processor without
SVM, DOSH boots, reports `hv status=unavailable`, and runs nothing.

## Hardware virtualization (`hv/`)

`hv` is the only module that knows which virtualization extension the
processor has. It exports:

- `Hv::enable()` — switch the extension on, or say why not.
- `Hv::create_vm(ram)` — a guest with that much memory, in real mode.
- `Vm::cpu`, `Vm::ram()` — guest registers and memory, for the capsule to
  read and edit between runs.
- `Vm::run() -> Exit` — execute guest instructions on the processor until
  the guest needs the supervisor: `Halt`, `Io`, `MemoryFault`, `Shutdown`,
  `Preempted`, `Nmi`, `Forbidden`.

The one backend is AMD SVM (`hv/svm.rs`). A guest is confined three ways:

- **Nested paging** maps exactly its own megabyte. The 64 KiB above 1 MiB
  maps back onto the first 64 KiB, which reproduces a PC with the A20 line
  off without trapping anything.
- **Intercepts** turn every port access, MSR access, control- and
  debug-register write, SVM instruction, and `HLT` into an exit.
- **Nothing else exists.** There is no emulated chipset to attack.

A real-mode guest runs at CPL 0 — that is what real mode is — so containment
comes from the two mechanisms above rather than from privilege levels.

The world switch saves and restores what `VMRUN` does not: the remaining
general registers, the hidden segment state (`VMLOAD`/`VMSAVE`), and the
x87/SSE state (`FXSAVE`/`FXRSTOR`), so capsules cannot see or disturb each
other's floating-point registers.

**Preemption.** The supervisor runs with interrupts disabled, but enters
guests with them enabled and with `V_INTR_MASKING` set, so the guest's own
interrupt flag has no say over the supervisor's clock. A tick during guest
execution forces an exit; the supervisor takes the interrupt at a point of
its choosing and moves to the next capsule. A program that disables
interrupts and spins forever costs the others nothing but their share of
time.

All guests share one address-space ID and the TLB is flushed when a
different guest is entered. Per-guest ASIDs are an optimisation for later.

### Why SVM first, and what it costs

The development machine is AMD, and QEMU's software processor emulates SVM
but not VT-x, so one backend serves both lab paths. The cost: **DOSH does
not yet run on Intel processors.** A VT-x backend needs "unrestricted
guest" for real mode and belongs in a sibling module behind the same
`Hv`/`Vm` surface. `Cpu`, `Segment` and `Exit` are already
vendor-neutral, and the service gate (below) was chosen to be.

## Capsules (`capsule.rs`)

A capsule is one isolated workload: a `Vm`, the DOS personality its program
talks to, its virtual devices, and its presentation endpoint. Its states are
running, waiting for console input, exited (with a code), and faulted (with
a reason).

`run_slice` gives a capsule the processor until it finishes, blocks, or the
clock ticks. Each exit from the guest is handled and the guest re-entered:

| Exit | Meaning | Handling |
| --- | --- | --- |
| `Halt` at a service gate | the program made a DOS or BIOS call | perform it (below) |
| `Halt` elsewhere | the program is idle in `HLT` | sleeps until the next clock tick; fault if interrupts are disabled |
| `Io` | port access | only the display status port (3DAh) is a device; elsewhere writes vanish and reads float high, as on an empty bus |
| `Preempted` | the clock ticked | nothing; the slice is over |
| `MemoryFault`, `Shutdown`, `Forbidden` | the guest left its envelope | fault, with the reason |

A fault ends only the capsule. Everything a capsule is lives in its `Vm`
and its `Dos`, which is what makes suspend, resume and snapshot tractable
later.

### Service gates

A DOS program reaches DOS and the BIOS with `INT n`, and is free to read,
replace and chain interrupt vectors. So DOSH does not intercept `INT`. The
vectors are real. Each initially points at a four-byte gate in the guest's
ROM segment:

```
F000:(n*4)   HLT     ; exit to the supervisor, which performs service n
             IRET    ; return to the caller, natively
```

The timer's gate, INT 08h, is BIOS code rather than a plain gate: after its
service it calls INT 1Ch through the vector, so a program's own tick hook
runs on every tick, as on a PC.

The processor delivers the interrupt itself, pushing a normal interrupt
frame. The capsule recognises the gate by its address, performs the service
against the guest's registers and memory, steps past the `HLT`, and resumes;
the guest's own `IRET` returns to the caller with the flags the service
left in the frame. A program that hooks a vector and chains to the old one
works exactly as on DOS, and processor exceptions arrive the same way.
`HLT` is a single byte that every virtualization extension can intercept,
so the gate carries no vendor-specific instruction.

A call that needs console input that has not arrived leaves the guest
parked on the gate with its state untouched. The capsule makes the same
call again when input appears. No supervisor stack is ever held on behalf
of a blocked guest.

While a call is parked, the guest's BIOS clock keeps advancing and one
pending INT 08h is recorded, but the guest does not execute, so its timer
handlers do not run until the call completes. Ticks that arrive during the
wait coalesce into that one pending INT 08h. This is an initial limitation,
not a design: a PC would run timer handlers during a wait. Lifting it needs
the blocked service to re-enter the guest, which the virtual interval timer
work will decide.

## DOS personality (`crates/dos`)

The DOS compatibility layer is a separate crate that knows nothing about
processors, hypervisors or transports. It is given the capsule's memory, the
16-bit register state at a gate, and a character device, and it returns
what to do next: resume, exit with a code, wait for input, or fault. DOS
data structures that programs can see — the vector table, the memory
control block chain, the PSP, the environment — are kept in guest memory in
their documented layouts.

Because it is plain `no_std` Rust with no hardware dependencies, it is
unit-tested on the build host. The contract it implements, and the rule
that unimplemented services fail loudly rather than being faked, are in
[dos-contract.md](dos-contract.md).

## Devices and presentation (`device.rs`, `present.rs`, `crates/term`)

Three layers, deliberately separate:

1. **DOS console services** (`crates/dos`): what `INT 21h` functions 01h,
   09h, 0Ah, 3Fh and friends mean — echo, line editing, Ctrl-Z. They read
   and write a `CharDevice`.
2. **The console device** (`device.rs`): the capsule's virtual console, two
   byte queues. It has no idea who, if anyone, is on the other end. The
   other virtual device is the display adapter's status register (port
   3DAh), a fixed retrace pattern that lets programs which poll for the
   retrace proceed.
3. **Presentation** (`present.rs`): an `Endpoint` carries a capsule to a
   transport. Today the transport is a polled 16550: COM1 is the
   supervisor log, and capsule *n* gets the next port along if the
   machine has one (or all share COM2 under `console mux`). A capsule with
   no endpoint runs anyway.

### The screen

The display is guest memory, not a device model. `crates/dos/src/video.rs`
keeps an 80×25 text buffer at `B800:0000` and the cursor and mode in the
BIOS data area; `INT 10h` operates on it, and programs may store into it
directly. `keyboard.rs` turns the console's byte stream into BIOS keys
(scan code and ASCII) for `INT 16h`, decoding the terminal's escape
sequences for arrows and function keys. Both live in `crates/dos` because
they are DOS/BIOS behaviour; neither knows about a transport.

A capsule starts with a plain byte-stream console. The first `INT 10h`
call other than teletype output marks its program as having a screen
(`Dos::screen_active`). From then on the capsule applies its console
output to the buffer, and each tick (and at exit) hands a `Snapshot` of
the buffer and cursor to its endpoint. The endpoint owns a
`dosh_term::Terminal`, which keeps what the far end last showed and emits
only the ANSI sequences that bring it up to date (code page 437 as UTF-8,
CGA colours mapped to ANSI). `crates/term` is `no_std`, takes cells in and
gives bytes out, and is unit-tested on the host.

Under `console mux` one serial line carries every capsule, one at a time;
see `present.rs` for the Ctrl-] switch and how a screen is redrawn in full
when a capsule is shown again.

A serial line has no notion of a session, and firmware prints on every port
it finds. So an endpoint announces itself with one line —
`--- DOSH capsule N ---` — and everything after it, in both directions, is
the capsule's bytes, untouched.

Replacing the serial endpoint with a network one changes `present.rs`; it
does not touch the DOS services or the capsule's execution. Attaching a
local display instead of a terminal means rendering the same `Snapshot`.

## Decisions that differ from the kickoff's suggestions

- **No NASM in the supervisor.** Rust's `global_asm!` covers the two pieces
  of assembly it needs (interrupt entry and the world switch), which keeps
  one toolchain and one build step. NASM builds the guest corpus.
- **No nightly Rust.** Stable 1.91, pinned.
- **The lab is a container**, and Python test tooling runs inside it with
  the distribution's pytest, rather than in a per-project virtualenv on the
  host. One environment to reproduce instead of two.
- **Work is provisioned from the boot volume, and from a persistent disk.**
  Drive C: is held in memory while a capsule runs and written back to the
  persistent disk when it exits. There is no network driver; remote
  operation goes over serial lines that the lab's QEMU puts on TCP.

## Known limits of the current design

- One processor. Application processors are never started.
- A fixed 64 MiB pool; exhausting it is a supervisor panic (logged, then
  power-off). Per-capsule accounting is partial: drive C: growth and
  console input backlog are bounded, nothing else needs to be yet.
- A capsule can be stopped from outside only on a supervisor that stays up
  (`kill`). Without `supervisor stay`, one that never exits keeps DOSH
  running forever, though it cannot starve the others; use `limit=`.
- The management channel has no authentication. Whoever reaches COM1
  operates the machine.
- The disk's files are read into memory once per change, and capsules share
  them until written; a program that rewrites a big file still copies it.
- The display is text mode only: one page, modes 2 and 3, no graphics and
  no blink or intensity distinction beyond what ANSI gives.
- Console input is polled once per slice; a serial peer sending faster
  than the UART FIFO drains, with no flow control, can lose bytes.
- Guests see the host's `CPUID` and time-stamp counter unfiltered.
- Supervisor page tables map everything read-write-execute.

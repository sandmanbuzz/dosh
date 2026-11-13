# The development laboratory

DOSH is developed against virtual bare metal. One container holds every
tool; one script enters it; one command proves the system works. No
physical machine is reserved, and any number of people or agents can work
at once without knowing about each other.

## Entering the lab

```sh
./lab                 # an interactive shell in the lab
./lab make test       # or any single command
```

`./lab` needs only `podman`. It builds the image from
`tools/lab/Containerfile` on first use, and again whenever that file
changes (the image is tagged with the file's hash). The checkout is mounted
at `/work`. If `/dev/kvm` is usable it is passed through.

What is inside: Debian 13 (base image pinned by digest), Rust 1.91.0 with
the `x86_64-unknown-uefi` target (pinned in the Containerfile and in
`rust-toolchain.toml`; keep them in step), NASM and bcc for the guest
corpus, mtools, QEMU and OVMF, Python 3 with pytest. Crate versions are
pinned by `Cargo.lock`. Debian packages are not pinned beyond the release,
so a rebuilt image may pick up point updates to QEMU or OVMF.

## Commands

| Command | What it does |
| --- | --- |
| `make efi` | Build the supervisor: `target/x86_64-unknown-uefi/release/dosh.efi` |
| `make guests` | Assemble and compile the guest corpus into `build/guests/` |
| `make check` | rustfmt, clippy with warnings denied, and the DOS crate's unit tests |
| `make test` | All of the above, then the acceptance suite |
| `python3 -m pytest tests -k NAME` | Selected acceptance tests |
| `tools/doshlab.py CAPSULE...` | Boot DOSH with these capsules and show consoles and log |

`tools/doshlab.py` options: `--accel kvm|tcg` picks the execution path;
`--attach` makes your terminal the first capsule's console; `--keep DIR`
saves the boot disk and logs of a run; `--raw` prints firmware output too.
Piped standard input is delivered to the first capsule once its session
opens.

## Two execution paths, three separate claims

| Path | QEMU | What a passing test shows |
| --- | --- | --- |
| `tcg` | software processor with emulated SVM | The supervisor's logic is right against an emulated processor. Runs anywhere. |
| `kvm` | KVM with nested SVM, `-cpu host` | The same, with guest instructions executing on the physical processor. Needs an AMD host with nested virtualization enabled. |

Every guest test runs on both. On a machine without usable KVM the `kvm`
half is reported as *skipped*, with the reason, never as passed.

Three facts are established separately and never inferred from each other:

1. **It builds.** `make efi`.
2. **It boots.** The log shows `boot stage=supervisor`.
3. **A guest ran.** The log shows that capsule's own `event=exit` and the
   capsule's console shows its output.

`test_booting_is_not_the_same_as_running_a_guest` pins the distinction
down: on a processor without SVM, DOSH boots, reports
`hv status=unavailable`, and runs nothing.

Nested virtualization is still not a physical machine. Bringing DOSH up on
real hardware is on the roadmap and has not been attempted.

## How a run works

`doshlab.run()` creates a private temporary directory and in it:

- builds a FAT disk image holding `\EFI\BOOT\BOOTX64.EFI`, the boot
  manifest `\DOSH\BOOT.CFG`, and drive C: under `\DOSH\C\`;
- copies the OVMF variable store;
- starts QEMU with that disk, COM1 written to a log file, and COM2–COM4
  each connected to a socket pair inherited by file descriptor.

It then collects console bytes until DOSH powers the machine off, parses
the supervisor's `[dosh]` lines into events, and returns everything. A
capsule's console capture starts after its session banner, so firmware
output is never mistaken for the guest's. Input is held back until the
banner too, because firmware would otherwise eat it.

Nothing is shared between runs: no fixed paths, ports or names. Tests can
run in parallel, and separate checkouts have separate build caches
(`target/`, `build/`, `.lab/`).

## Adding to the corpus

1. Write the smallest program that shows the behaviour:
   `guests/name.asm`, assembled as a flat binary. `mz.inc` writes an MZ
   header for multi-segment programs. C sources go through bcc; see `WC.COM`
   in `guests/Makefile`.
2. Add it to `guests/Makefile`.
3. Add an acceptance test in `tests/test_acceptance.py` that checks console
   output and exit code.
4. If the DOS behaviour has edge cases, add a unit test next to the others
   in `crates/dos/src/tests.rs`; they run in milliseconds.

For a third-party binary, the loop is the same, starting from the fault
line the supervisor prints when the program asks for something outside the
contract.

## Debugging

- `tools/doshlab.py --raw --keep build/run PROGRAM` keeps the disk image
  and the full COM1 log.
- A supervisor panic or unexpected processor trap is logged
  (`panic at=... message=...`, `trap vector=... rip=...`) before power-off.
- A run that never powers off times out; the log so far is in the test
  failure. The usual cause is a capsule waiting for input that was not
  sent.

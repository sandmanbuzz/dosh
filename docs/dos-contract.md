# The DOS compatibility contract

This is what a program running in a DOSH capsule can rely on today. It is
the first rung of the compatibility ladder — the "360K" class: conventional
`.COM` and `.EXE` programs that take arguments, read and write files, talk
to a line-oriented console, and exit.

## The rule

**A service DOSH does not implement is never faked.** A call outside this
contract stops the capsule with a fault that names it:

```
capsule id=0 event=fault at=1005:0757 reason="unimplemented service INT 21h AH=59h AL=02h"
```

That is how the contract grows: run a real program, read what it asked for,
implement that, add the program (or a small one like it) to the corpus.
The line above is real; it came from the first C program tried, and
function 59h was implemented because of it. A list of supported interrupts
is not the goal. Programs that work are.

Each item below is exercised by a unit test in `crates/dos/src/tests.rs`, a
guest in `guests/` with an acceptance test, or both.

## The machine a program finds

| Region | Contents |
| --- | --- |
| `0000:0000` | Interrupt vector table. All 256 vectors are valid and point at service gates. |
| `0040:0000` | BIOS data area: equipment word, 640 KB conventional memory, 80×25 colour text mode. |
| `1000:0000`–`9FFF:000F` | The DOS memory arena, a standard MCB chain. Programs load above 64 KiB. |
| `A000:0000`–`EFFF:000F` | Plain memory. No display adapter is behind it. |
| `F000:0000` | Service gates (see [architecture.md](architecture.md)). |
| above `FFFF:000F` | Wraps to `0000:0000`: the A20 line is off. |

The processor is whatever the machine has, in real mode. 32-bit registers
and instructions are available. Port I/O reaches no devices: writes are
discarded and reads return all ones. Control registers, MSRs and debug
registers are off limits, and touching them faults the capsule.

The only hardware interrupt delivered is the clock tick, `INT 08h`. There is
no keyboard interrupt yet. The BIOS clock is kept current in the
BIOS data area (0040:006Ch, the tick count, and 0040:0070h, the midnight
flag), and `INT 1Ah AH=00h` reads it. The BIOS clock advances at its own
rate, about 18.2 a second of elapsed time, and each advance is delivered as
`INT 08h` to any program with interrupts enabled, through the IVT. The
supervisor's scheduling quantum is not visible to the guest. A `HLT` outside
a service gate sleeps the program until the next tick, and faults if
interrupts are disabled, because nothing would ever wake it.

## Program loading

- The program is a file on drive C:, loaded by content: an `MZ` (or `ZM`)
  signature means an EXE, anything else a COM.
- **COM**: one segment, image at offset 100h, all free memory allocated to
  it, `SS:SP` at the top of the segment with a zero word pushed so that a
  near `RET` terminates the program. Images over 65,024 bytes are refused.
- **MZ**: header parsed, load module copied, relocations applied, memory
  sized from the header's minimum and maximum allocation, `CS:IP` and
  `SS:SP` set from the header. Data past the load module (overlays, debug
  information) is ignored. Inconsistent headers are refused.
- **PSP**: `INT 20h` at 00h, top of memory at 02h, the CP/M call stub at
  05h, saved vectors 22h–24h, parent PSP (itself: the program is the root
  process), a 20-entry job file table, the environment segment at 2Ch,
  `INT 21h / RETF` at 50h, blank FCBs, and the command tail at 80h.
- **Environment**: `PATH=C:\`, `COMSPEC=C:\COMMAND.COM`, then the
  program's full path, as DOS 3 and later provide.
- **Registers at entry**: `DS = ES = PSP`, `AX = 0`, interrupts enabled.

The command tail is passed exactly as written after the program name in the
boot manifest, leading space included, up to 126 characters.

## INT 21h

Errors are reported as DOS does: carry set and the error code in `AX`.

| AH | Function | Notes |
| --- | --- | --- |
| 00h | Terminate | Return code 0. |
| 01h | Read character with echo | Blocks until input arrives. |
| 02h | Write character | |
| 06h | Direct console I/O | `DL = FFh` reads without waiting, reporting through ZF. |
| 07h, 08h | Read character without echo | No Ctrl-C checking on either. |
| 09h | Write `$`-terminated string | |
| 0Ah | Buffered line input | Line editing: backspace, bell when full. Echoes CR on Enter. |
| 0Bh | Input status | |
| 19h | Current drive | Always C:. |
| 1Ah, 2Fh | Set / get disk transfer address | Stored; nothing uses the DTA yet. |
| 25h, 35h | Set / get interrupt vector | Act on the real vector table. |
| 30h | DOS version | 5.00. |
| 3Ch | Create file | Truncates an existing file. Attributes are ignored. |
| 3Dh | Open file | Access mode honoured; sharing bits ignored. |
| 3Eh | Close | |
| 3Fh | Read | From the console: a whole edited line, echoed, CR LF appended; a line starting with Ctrl-Z reads as end of file. |
| 40h | Write | Zero bytes sets the file size. A full disk is a short write. |
| 41h | Delete | |
| 42h | Seek | From start (unsigned), current or end (signed). |
| 44h | IOCTL | Subfunction 00h only: device information. |
| 48h, 49h, 4Ah | Allocate, free, resize memory | First fit; free blocks coalesce. A failed grow leaves the block at its largest possible size, as DOS does. |
| 4Ch | Exit with return code | Ends the capsule; the code is reported in the supervisor log. |
| 51h, 62h | Get PSP | |
| 59h | Extended error | Exact code; conventional class, action and locus. |

## Other interrupts

| Interrupt | Function |
| --- | --- |
| `INT 10h`, `AH = 0Eh` | Teletype output, to the console. No other video service. |
| `INT 11h`, `INT 12h` | Equipment word; conventional memory size. |
| `INT 20h` | Terminate. |
| `INT 29h` | Fast console output. |
| `INT 00h`–`07h` | Processor exceptions. Unless the program has installed its own handler, the capsule faults. |

## Drive C:

- One flat root directory, held in memory, private to the capsule. Every
  capsule starts with its own copy of the provisioned files.
- Names are case-insensitive. `C:` and a leading `\` are accepted; any
  other drive or any subdirectory is "path not found".
- `CON` and `NUL` are devices, with or without an extension. Standard
  handles 0–2 are the console; 3 and 4 (`AUX`, `PRN`) are null devices.
- Twenty handles per process.
- 4 MiB of space beyond the provisioned files.
- Changes are not written back to the boot volume.

## The console

One character device per capsule, a byte stream in each direction. DOS
output is passed through untranslated: a program that writes CR LF sends
CR LF. On input, CR is Enter, backspace and DEL erase, and a LF directly
from the stream is ignored so that peers which send CR LF work. There is no
type-ahead flush and no Ctrl-C or Ctrl-Break handling.

## Known gaps a real program will hit soon

In rough order of expected demand: date and time (2Ah, 2Ch, `INT 1Ah AH=02h`);
find first/next (4Eh, 4Fh) and file attributes and timestamps (43h, 57h);
directories (39h–3Bh, 47h); handle duplication and redirection (45h, 46h);
`EXEC` and child processes (4Bh, 4Dh); FCB file I/O; keyboard and video
BIOS services (`INT 16h`, `INT 10h` beyond teletype); Ctrl-C and
critical-error handling (`INT 23h`, `INT 24h`); country information (38h);
terminate-and-stay-resident (31h).

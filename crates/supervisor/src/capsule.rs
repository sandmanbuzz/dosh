//! Execution capsules: one isolated DOS workload each.
//!
//! A capsule owns a hardware-virtualized guest ([`hv::Vm`]), the DOS
//! personality that guest's program talks to, and its virtual devices. The
//! supervisor drives it one step at a time: run the guest until it exits to
//! us, deal with the reason, and say whether it may run again.

use alloc::format;
use alloc::string::String;

use dosh_dos::{firmware, Dos, Fault, Mem, Outcome, Regs, Volume, RAM_SIZE};

use crate::boot::CapsuleSpec;
use crate::device::Console;
use crate::hv::{self, Cpu, Exit, Segment};
use crate::log;
use crate::platform::timer;
use crate::present::Endpoint;

/// Space on a capsule's drive C: beyond the files it was provisioned with.
const DISK_ROOM: usize = 4 << 20;
/// The clock tick, delivered to the guest as the PC's IRQ 0: INT 08h.
const TICK_VECTOR: u8 = 0x08;
/// The processor's interrupt-enable flag.
const FLAG_IF: u64 = 0x200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// The guest can execute.
    Running,
    /// The guest is inside a DOS call that needs console input.
    WaitingInput,
    /// The guest executed HLT with interrupts enabled and waits for a tick.
    Halted,
    Exited(u8),
    Faulted,
}

pub struct Capsule {
    id: usize,
    vm: hv::Vm,
    dos: Dos,
    console: Console,
    endpoint: Endpoint,
    state: State,
    /// Times the guest has left native execution for the supervisor.
    exits: u64,
    /// How many of those were the clock taking the processor away.
    preemptions: u64,
    /// Port accesses that reached no device.
    stray_io: u64,
}

fn regs_of(cpu: &Cpu) -> Regs {
    Regs {
        ax: cpu.gprs.rax as u16,
        bx: cpu.gprs.rbx as u16,
        cx: cpu.gprs.rcx as u16,
        dx: cpu.gprs.rdx as u16,
        si: cpu.gprs.rsi as u16,
        di: cpu.gprs.rdi as u16,
        bp: cpu.gprs.rbp as u16,
        sp: cpu.gprs.rsp as u16,
        ip: cpu.rip as u16,
        flags: cpu.rflags as u16,
        cs: cpu.cs.selector,
        ds: cpu.ds.selector,
        es: cpu.es.selector,
        ss: cpu.ss.selector,
    }
}

/// Write 16-bit state back, leaving the upper halves of wider registers and
/// the hidden state of unchanged segments alone.
fn apply(cpu: &mut Cpu, regs: &Regs) {
    let low = |wide: &mut u64, value: u16| *wide = (*wide & !0xFFFF) | value as u64;
    low(&mut cpu.gprs.rax, regs.ax);
    low(&mut cpu.gprs.rbx, regs.bx);
    low(&mut cpu.gprs.rcx, regs.cx);
    low(&mut cpu.gprs.rdx, regs.dx);
    low(&mut cpu.gprs.rsi, regs.si);
    low(&mut cpu.gprs.rdi, regs.di);
    low(&mut cpu.gprs.rbp, regs.bp);
    low(&mut cpu.gprs.rsp, regs.sp);
    low(&mut cpu.rflags, regs.flags);
    cpu.rip = regs.ip as u64;
    for (seg, selector, code) in [
        (&mut cpu.cs, regs.cs, true),
        (&mut cpu.ds, regs.ds, false),
        (&mut cpu.es, regs.es, false),
        (&mut cpu.ss, regs.ss, false),
    ] {
        if seg.selector != selector || seg.attr == 0 {
            *seg = Segment::real(selector, code);
        }
    }
}

impl Capsule {
    /// Build a capsule and load its program. A capsule whose program cannot
    /// be loaded is created already faulted, so it is accounted for like any
    /// other.
    pub fn launch(id: usize, hv: &hv::Hv, spec: CapsuleSpec, endpoint: Endpoint) -> Self {
        let mut vm = hv.create_vm(RAM_SIZE);
        let stocked: usize = spec.files.iter().map(|(_, data)| data.len()).sum();
        let mut volume = Volume::new(stocked + DISK_ROOM);
        for (name, data) in spec.files {
            volume.put(&name, data);
        }
        let mut mem = Mem::new(vm.ram());
        let mut dos = Dos::new(&mut mem, volume);
        let loaded = dos.exec(&mut mem, &spec.program, &spec.tail);

        log!("capsule id={id} event=create program={} console={}", spec.program, endpoint.name());
        let mut capsule = Self {
            id,
            vm,
            dos,
            console: Console::default(),
            endpoint,
            state: State::Running,
            exits: 0,
            preemptions: 0,
            stray_io: 0,
        };
        match loaded {
            Ok(regs) => {
                let cpu = &mut capsule.vm.cpu;
                cpu.fs = Segment::real(0, false);
                cpu.gs = Segment::real(0, false);
                apply(cpu, &regs);
                log!("capsule id={id} event=start entry={:04X}:{:04X}", regs.cs, regs.ip);
            }
            Err(e) => capsule.fault_here(format!("cannot load {}: {e:?}", spec.program)),
        }
        capsule
    }

    pub fn is_live(&self) -> bool {
        matches!(self.state, State::Running | State::WaitingInput | State::Halted)
    }

    pub fn exit_code(&self) -> Option<u8> {
        match self.state {
            State::Exited(code) => Some(code),
            _ => None,
        }
    }

    /// Give the capsule a turn: it runs until it finishes, blocks, or the
    /// clock ticks. Returns whether it did anything.
    pub fn run_slice(&mut self) -> bool {
        timer::accept(); // a tick already pending belongs to nobody's slice
        let started = timer::ticks();
        let mut ran = false;
        loop {
            ran |= self.step();
            if self.state != State::Running || timer::ticks() != started {
                return ran;
            }
        }
    }

    fn step(&mut self) -> bool {
        self.endpoint.pump(&mut self.console);
        // The clock advances whether or not the guest runs, so a capsule that
        // is waiting for input still sees midnight.
        // The guest's clock is its own BIOS count, which follows elapsed time,
        // not the supervisor's quantum: one INT 08h per count advance.
        if self.dos.set_clock(&mut Mem::new(self.vm.ram()), timer::ticks()) {
            self.vm.post_interrupt(TICK_VECTOR);
            if self.state == State::Halted {
                // The interrupt is taken after the HLT that put us to sleep.
                self.vm.cpu.rip = (self.vm.cpu.rip as u16).wrapping_add(1) as u64;
                self.state = State::Running;
            }
        }
        let ran = match self.state {
            State::Running => {
                let exit = self.vm.run();
                self.exits += 1;
                self.handle(exit);
                true
            }
            State::WaitingInput if !self.console.rx.is_empty() => {
                self.service();
                true
            }
            _ => false,
        };
        self.endpoint.pump(&mut self.console);
        ran
    }

    fn handle(&mut self, exit: Exit) {
        match exit {
            Exit::Halt => self.service(),
            Exit::Io { string: true, port, .. } => self.fault_here(format!("string I/O on port {port:#x}")),
            Exit::Io { input, bytes, next_rip, .. } => {
                // No devices are attached to the capsule's I/O bus yet. As on
                // a PC, writes to an empty address vanish and reads float high.
                if input {
                    self.vm.cpu.gprs.rax |= (1u64 << (8 * bytes.min(4))) - 1;
                }
                self.vm.cpu.rip = next_rip;
                self.stray_io += 1;
            }
            Exit::MemoryFault { address } => self.fault_here(format!("access to unmapped memory at {address:#x}")),
            Exit::Shutdown => self.fault_here("triple fault".into()),
            Exit::Preempted => self.preemptions += 1,
            Exit::Forbidden { code, info } => {
                self.fault_here(format!("forbidden operation (exit code {code:#x}, info {info:#x})"))
            }
        }
    }

    /// The guest is halted at `cs:ip`. If that is a service gate, perform
    /// the service.
    fn service(&mut self) {
        let mut regs = regs_of(&self.vm.cpu);
        let Some(vector) = firmware::gate_vector(regs.cs, regs.ip) else {
            return self.guest_halt();
        };
        let mut mem = Mem::new(self.vm.ram());
        // The gate was entered by an interrupt, whose frame holds the address
        // to return to: just past an INT instruction, or at a faulting one.
        let origin = (mem.r16(regs.ss, regs.sp.wrapping_add(2)), mem.r16(regs.ss, regs.sp));
        match self.dos.interrupt(vector, &mut regs, &mut mem, &mut self.console) {
            Outcome::Resume => {
                regs.ip += 1; // on to the gate's IRET
                apply(&mut self.vm.cpu, &regs);
                self.state = State::Running;
            }
            Outcome::WaitInput => self.state = State::WaitingInput,
            Outcome::Exit(code) => {
                self.state = State::Exited(code);
                log!(
                    "capsule id={} event=exit code={code} vmexits={} preempted={} stray_io={}",
                    self.id,
                    self.exits,
                    self.preemptions,
                    self.stray_io
                );
            }
            Outcome::Fault(fault) => {
                let what = match fault {
                    Fault::Unimplemented { vector, ah, al } => {
                        format!("unimplemented service INT {vector:02X}h AH={ah:02X}h AL={al:02X}h")
                    }
                    Fault::Exception { vector } => format!("unhandled processor exception {vector}"),
                };
                self.fault(origin, what);
            }
        }
    }

    /// The guest executed HLT outside a service gate. With interrupts enabled
    /// the next tick wakes it; with them disabled nothing ever would.
    fn guest_halt(&mut self) {
        if self.vm.cpu.rflags & FLAG_IF == 0 {
            return self.fault_here("HLT with interrupts disabled".into());
        }
        if self.vm.interrupt_pending() {
            // An interrupt is already posted, and interrupts are held off
            // only for the instruction after STI: take it after this HLT.
            self.vm.cpu.rip = (self.vm.cpu.rip as u16).wrapping_add(1) as u64;
            return;
        }
        self.state = State::Halted;
    }

    /// Stop the capsule for good, blaming the instruction the guest is at.
    fn fault_here(&mut self, reason: String) {
        let cpu = &self.vm.cpu;
        self.fault((cpu.cs.selector, cpu.rip as u16), reason);
    }

    fn fault(&mut self, at: (u16, u16), reason: String) {
        self.state = State::Faulted;
        log!(
            "capsule id={} event=fault at={:04X}:{:04X} vmexits={} reason=\"{reason}\"",
            self.id,
            at.0,
            at.1,
            self.exits
        );
    }
}

//! Execution capsules: one isolated DOS workload each.
//!
//! A capsule owns a hardware-virtualized guest ([`hv::Vm`]), the DOS
//! personality that guest's program talks to, and its virtual devices. The
//! supervisor drives it one step at a time: run the guest until it exits to
//! us, deal with the reason, and say whether it may run again.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use dosh_dos::{firmware, video, DateTime, Dos, Fault, Mem, Outcome, Regs, Volume, RAM_SIZE};

use crate::boot::CapsuleSpec;
use crate::device::{Cga, Console};
use crate::hv::{self, Cpu, Exit, Segment};
use crate::log;
use crate::platform::{self, timer};
use crate::present::Endpoint;

/// Space on a capsule's drive C: beyond the files it was provisioned with.
const DISK_ROOM: usize = 4 << 20;
/// The clock tick, delivered to the guest as the PC's IRQ 0: INT 08h.
const TICK_VECTOR: u8 = firmware::TIMER_VECTOR as u8;
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
    /// Stopped by the operator.
    Killed,
}

impl State {
    fn is_live(self) -> bool {
        matches!(self, State::Running | State::WaitingInput | State::Halted)
    }
}

/// What a capsule needs only while it can still run. Dropped on retirement,
/// which hands the guest's memory and its drive back to the supervisor.
struct Machine {
    vm: hv::Vm,
    dos: Dos,
}

pub struct Capsule {
    id: usize,
    program: String,
    /// `None` once the capsule has finished and been shown for the last time.
    machine: Option<Machine>,
    /// Set while the operator has the capsule suspended: the tick count then.
    suspended_at: Option<u64>,
    /// Ticks the capsule has spent suspended. Its clock does not count them.
    paused: u64,
    console: Console,
    cga: Cga,
    endpoint: Endpoint,
    state: State,
    /// Times the guest has left native execution for the supervisor.
    exits: u64,
    /// How many of those were the clock taking the processor away.
    preemptions: u64,
    /// Port accesses that reached no device.
    stray_io: u64,
    /// Clock ticks of processor time the guest may use before it is stopped.
    tick_budget: Option<u64>,
    /// Whether the endpoint has switched from the byte stream to the screen.
    screen_shown: bool,
    /// The provisioned files, by name and content hash, to tell what the program changed.
    stocked: Vec<(String, u32)>,
    /// The clock tick at which the screen was last drawn.
    drawn_at: u64,
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
    /// Build a capsule with `files` on its drive C: and load its program. A
    /// capsule whose program cannot be loaded is created already faulted, so it is accounted for like any
    /// other.
    pub fn launch(
        id: usize,
        hv: &hv::Hv,
        spec: CapsuleSpec,
        files: Vec<(String, Vec<u8>)>,
        endpoint: Endpoint,
        wall_clock: Option<DateTime>,
    ) -> Self {
        let mut vm = hv.create_vm(RAM_SIZE);
        let stocked: usize = files.iter().map(|(_, data)| data.len()).sum();
        let mut volume = Volume::new(stocked + DISK_ROOM);
        for (name, data) in files {
            volume.put(&name, data);
        }
        let stocked = volume.names().map(|n| (n.to_string(), fnv1a(volume.get(n).unwrap_or_default()))).collect();
        let mut mem = Mem::new(vm.ram());
        let mut dos = Dos::new(&mut mem, volume);
        if let Some(at_boot) = wall_clock {
            dos.set_calendar(at_boot);
        }
        let loaded = dos.exec(&mut mem, &spec.program, &spec.tail);

        log!("capsule id={id} event=create program={} console={}", spec.program, endpoint.name());
        let mut capsule = Self {
            id,
            program: spec.program.clone(),
            machine: Some(Machine { vm, dos }),
            suspended_at: None,
            paused: 0,
            console: Console::default(),
            cga: Cga::default(),
            endpoint,
            state: State::Running,
            exits: 0,
            preemptions: 0,
            tick_budget: spec.cpu_limit.map(|seconds| seconds.saturating_mul(timer::TICK_HZ as u64)),
            stray_io: 0,
            screen_shown: false,
            stocked,
            drawn_at: 0,
        };
        match loaded {
            Ok(regs) => {
                let cpu = &mut capsule.machine.as_mut().expect("just built").vm.cpu;
                cpu.fs = Segment::real(0, false);
                cpu.gs = Segment::real(0, false);
                apply(cpu, &regs);
                log!("capsule id={id} event=start entry={:04X}:{:04X}", regs.cs, regs.ip);
            }
            Err(e) => capsule.fault_here(format!("cannot load {}: {e:?}", spec.program)),
        }
        capsule
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// Whether the capsule has a program that could still run, suspended or not.
    pub fn is_live(&self) -> bool {
        self.state.is_live()
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended_at.is_some()
    }

    pub fn exit_code(&self) -> Option<u8> {
        match self.state {
            State::Exited(code) => Some(code),
            _ => None,
        }
    }

    /// One word for what the capsule is doing, for the operator.
    pub fn status(&self) -> &'static str {
        match self.state {
            _ if self.is_suspended() && self.is_live() => "suspended",
            State::Running => "running",
            State::WaitingInput => "waiting",
            State::Halted => "idle",
            State::Exited(_) => "exited",
            State::Faulted => "faulted",
            State::Killed => "killed",
        }
    }

    /// Stop scheduling the capsule and freeze its clock. Returns whether it
    /// was running to be suspended.
    pub fn suspend(&mut self) -> bool {
        if !self.is_live() || self.is_suspended() {
            return false;
        }
        self.suspended_at = Some(timer::ticks());
        log!("capsule id={} event=suspend", self.id);
        true
    }

    /// Undo [`suspend`](Self::suspend). The guest's clock carries on from
    /// where it stopped, as if the machine had been paused.
    pub fn resume(&mut self) -> bool {
        let Some(since) = self.suspended_at.take() else { return false };
        self.paused += timer::ticks() - since;
        log!("capsule id={} event=resume", self.id);
        true
    }

    /// Stop the capsule for good, whatever it is doing.
    pub fn kill(&mut self) -> bool {
        if !self.is_live() {
            return false;
        }
        self.state = State::Killed;
        self.suspended_at = None;
        log!("capsule id={} event=kill", self.id);
        self.retire();
        true
    }

    /// The capsule has finished and shown its last. Give back its guest memory
    /// and drive. The endpoint stays: on a shared line it still owns the
    /// Ctrl-] handling while it is the capsule shown.
    pub fn retire(&mut self) {
        self.present();
        self.machine = None;
    }

    pub fn is_retired(&self) -> bool {
        self.machine.is_none()
    }

    /// Give the capsule a turn: it runs until it finishes, blocks, or the
    /// clock ticks. Returns whether it did anything.
    pub fn run_slice(&mut self) -> bool {
        if self.is_suspended() || self.machine.is_none() {
            // Not running, but on a shared line it may be the capsule shown,
            // and then it still has to hear the key that moves the line.
            self.endpoint.pump(&mut self.console);
            return false;
        }
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
        let Some(Machine { vm, dos }) = self.machine.as_mut() else { return false };
        // The clock advances whether or not the guest runs, so a capsule that
        // is waiting for input still sees midnight.
        // The guest's clock is its own BIOS count, which follows elapsed time,
        // not the supervisor's quantum: one INT 08h per count advance.
        if dos.set_clock(&mut Mem::new(vm.ram()), timer::ticks() - self.paused) {
            vm.post_interrupt(TICK_VECTOR);
            if self.state == State::Halted {
                // The interrupt is taken after the HLT that put us to sleep.
                vm.cpu.rip = (vm.cpu.rip as u16).wrapping_add(1) as u64;
                self.state = State::Running;
            }
        }
        let ran = match self.state {
            State::Running => {
                let exit = vm.run();
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
        self.present();
        ran
    }

    /// Carry the console to the endpoint. A capsule whose program has taken
    /// over the display is drawn, at the clock's pace and when it ends; its
    /// console output lands on that screen rather than in the byte stream.
    fn present(&mut self) {
        if let Some(Machine { vm, dos }) = self.machine.as_mut() {
            if dos.screen_active() {
                if !self.screen_shown {
                    self.endpoint.pump(&mut self.console); // the stream so far
                    self.screen_shown = true;
                }
                let live = self.state.is_live();
                let mut mem = Mem::new(vm.ram());
                dos.draw_console(&mut mem, self.console.tx.drain(..));
                let ticks = timer::ticks();
                if ticks != self.drawn_at || !live {
                    self.drawn_at = ticks;
                    self.endpoint.show(&video::snapshot(&mem));
                }
            }
        }
        self.endpoint.pump(&mut self.console);
    }

    fn handle(&mut self, exit: Exit) {
        let Some(Machine { vm, .. }) = self.machine.as_mut() else { return };
        match exit {
            Exit::Halt => self.service(),
            Exit::Io { string: true, port, .. } => self.fault_here(format!("string I/O on port {port:#x}")),
            Exit::Io { input, bytes, port, next_rip, .. } => {
                // No devices are attached to the capsule's I/O bus yet. As on
                // a PC, writes to an empty address vanish and reads float high.
                if input && port == Cga::STATUS_PORT && bytes == 1 {
                    let rax = &mut vm.cpu.gprs.rax;
                    *rax = *rax & !0xFF | self.cga.status() as u64;
                } else {
                    if input {
                        vm.cpu.gprs.rax |= (1u64 << (8 * bytes.min(4))) - 1;
                    }
                    self.stray_io += 1;
                }
                vm.cpu.rip = next_rip;
            }
            Exit::MemoryFault { address } => self.fault_here(format!("access to unmapped memory at {address:#x}")),
            Exit::Shutdown => self.fault_here("triple fault".into()),
            Exit::Preempted => {
                self.preemptions += 1;
                // A tick that finds the guest running is a tick it used.
                if self.tick_budget.is_some_and(|budget| self.preemptions >= budget) {
                    self.fault_here(format!(
                        "watchdog: used its {} s of processor time",
                        self.preemptions / timer::TICK_HZ as u64
                    ));
                }
            }
            Exit::Nmi => log!("capsule id={} event=nmi total={}", self.id, platform::nmis()),
            Exit::Forbidden { code, info } => {
                self.fault_here(format!("forbidden operation (exit code {code:#x}, info {info:#x})"))
            }
        }
    }

    /// What the program did to its drive C: since launch: every file it
    /// created or changed, and the names of those it deleted. Logs each
    /// change with a hash of the contents, which is how a program's output is
    /// observed, then hands the changes to whoever persists them.
    pub fn changes(&self) -> Changes {
        let mut changes = Changes::default();
        let Some(Machine { dos, .. }) = self.machine.as_ref() else { return changes };
        let volume = dos.volume();
        for name in volume.names() {
            let data = volume.get(name).unwrap_or_default();
            let hash = fnv1a(data);
            if !self.stocked.iter().any(|(n, h)| n == name && *h == hash) {
                log!("file capsule={} name={name} size={} fnv1a={hash:08x}", self.id, data.len());
                changes.written.push((name.to_string(), data.to_vec()));
            }
        }
        for (name, _) in &self.stocked {
            if volume.get(name).is_none() {
                log!("file capsule={} name={name} deleted=yes", self.id);
                changes.deleted.push(name.clone());
            }
        }
        changes
    }

    /// The guest is halted at `cs:ip`. If that is a service gate, perform
    /// the service.
    fn service(&mut self) {
        let Some(Machine { vm, dos }) = self.machine.as_mut() else { return };
        let mut regs = regs_of(&vm.cpu);
        let Some(vector) = firmware::gate_vector(regs.cs, regs.ip) else {
            return self.guest_halt();
        };
        let mut mem = Mem::new(vm.ram());
        // The gate was entered by an interrupt, whose frame holds the address
        // to return to: just past an INT instruction, or at a faulting one.
        let origin = (mem.r16(regs.ss, regs.sp.wrapping_add(2)), mem.r16(regs.ss, regs.sp));
        match dos.interrupt(vector, &mut regs, &mut mem, &mut self.console) {
            Outcome::Resume => {
                regs.ip += 1; // on to the gate's IRET
                apply(&mut vm.cpu, &regs);
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
        let Some(Machine { vm, .. }) = self.machine.as_mut() else { return };
        if vm.cpu.rflags & FLAG_IF == 0 {
            return self.fault_here("HLT with interrupts disabled".into());
        }
        if vm.interrupt_pending() {
            // An interrupt is already posted, and interrupts are held off
            // only for the instruction after STI: take it after this HLT.
            vm.cpu.rip = (vm.cpu.rip as u16).wrapping_add(1) as u64;
            return;
        }
        self.state = State::Halted;
    }

    /// Stop the capsule for good, blaming the instruction the guest is at.
    fn fault_here(&mut self, reason: String) {
        let Some(Machine { vm, .. }) = self.machine.as_ref() else { return };
        let at = (vm.cpu.cs.selector, vm.cpu.rip as u16);
        self.fault(at, reason);
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

/// What a finished capsule's program left different on its drive C:.
#[derive(Default)]
pub struct Changes {
    pub written: Vec<(String, Vec<u8>)>,
    pub deleted: Vec<String>,
}

/// 32-bit FNV-1a: a cheap content fingerprint for the log.
fn fnv1a(data: &[u8]) -> u32 {
    data.iter().fold(0x811C_9DC5, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

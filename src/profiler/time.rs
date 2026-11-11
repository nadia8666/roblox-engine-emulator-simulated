//! Host time (`--profile time`): where the run thread's time went, sampled each time the ticker (luau_profile.rs, a
//! millisecond a tick) moved, and booked to what ran since the sample before:
//! - at a safepoint of the VM (a call, a return, a loop edge): the Luau stack there;
//! - when the interpreter is left (a handler returned or yielded): the Luau that ran last;
//! - when it is entered from outside: the engine's step that was open (the animator, the joints, physics, the recorder),
//!   else `(ree engine: outside scripts)` (the scheduler between handlers, events delivered);
//! - at the end of an Instance API call: the call, on the line that made it (its C frame innermost).
//!
//! Each sample carries the run thread's CPU cycles since the sample before (QueryThreadCycleTime, converted to milliseconds by
//! the cycle rate measured at the start) and the wall-clock ticks: a sample's CPU weight leaves out the time the thread was not
//! running (another process preempted it), which ticks count. Where the thread's cycles cannot be read, the CPU weight is the
//! wall time. A booking point first compares the ticker with the tick it last saw (one atomic load each), so a safepoint that
//! finds the ticker where it was costs that compare and nothing more.

use rustc_hash::FxHashMap;

use super::{cpu, memory, report};

/// What was booked where: (phase, stack) -> (ticks, CPU cycles)
pub struct Time {
    pub stacks: memory::Stacks,
    pub samples: FxHashMap<(u32, u32), (u64, u64)>,
    /// The same at a safepoint or an Instance API call, by the instruction the innermost Luau frame was at, and whether a C
    /// function ran since the safepoint before it: (function, pc, after a C function) -> (ticks, CPU cycles). The code since that
    /// safepoint is what the sample stands for (an inlined copy's share), and a C function's time when one ran in it
    pub pcs: FxHashMap<(i32, i32, bool), (u64, u64)>,
    /// The tick and the thread's cycles at the last booking
    seen_ticks: u64,
    seen_cycles: u64,
    /// The Luau stack the last safepoint found (what ran when the interpreter is left after its frames returned)
    last_stack: Option<u32>,
    /// The engine's steps open, innermost last (their frames' stacks)
    regions: Vec<u32>,
    /// `(ree engine: outside scripts)`
    outside: u32,
    /// Cycles a millisecond of the run thread's CPU time (0 when the thread's cycles cannot be read)
    pub cycles_per_ms: f64,
    /// Ticks booked before the VM ran (none: the profile starts with it), kept for the first booking
    pub base: u64,
}

impl Time {
    /// Starts sampling at tick `base`: measures the cycle rate first (a 5 ms spin), so the thread's cycles convert to milliseconds
    pub fn new(base: u64) -> Time {
        let cycles_per_ms = cpu::cycles_per_ms();
        let mut stacks = memory::Stacks::default();
        let outside = stacks.intern(&[(report::outside_function("ree engine: outside scripts"), -1)]);
        Time { stacks, samples: FxHashMap::default(), pcs: FxHashMap::default(), seen_ticks: base, seen_cycles: cpu::thread_cycles(), last_stack: None, regions: Vec::new(), outside, cycles_per_ms, base }
    }

    /// The ticks and cycles since the last booking, and the booking's new start
    fn take(&mut self, ticks: u64) -> (u64, u64) {
        let cycles = cpu::thread_cycles();
        let elapsed = (ticks.saturating_sub(self.seen_ticks), cycles.saturating_sub(self.seen_cycles));
        self.seen_ticks = self.seen_ticks.max(ticks);
        self.seen_cycles = cycles.max(self.seen_cycles);
        elapsed
    }

    fn add(&mut self, phase: u32, stack: u32, (ticks, cycles): (u64, u64)) {
        if ticks > 0 || cycles > 0 {
            let entry = self.samples.entry((phase, stack)).or_default();
            entry.0 += ticks;
            entry.1 += cycles;
        }
    }

    /// The booking's instruction: the innermost Luau frame's
    fn add_pc(&mut self, state: *mut mlua::ffi::lua_State, (ticks, cycles): (u64, u64)) {
        if ticks == 0 && cycles == 0 {
            return;
        }
        if let Some((function, pc)) = unsafe { super::vm::pc(state) } {
            let entry = self.pcs.entry((function, pc, crate::luau_profile::c_called())).or_default();
            entry.0 += ticks;
            entry.1 += cycles;
        }
    }

    /// Where engine time goes now: the innermost open step, else outside scripts
    fn outside_now(&self) -> u32 {
        self.regions.last().copied().unwrap_or(self.outside)
    }

    /// A safepoint where the ticker moved: what ran since the last booking was this stack's
    ///
    /// # Safety
    /// `state` is a live thread of the VM the profile runs on.
    pub unsafe fn safepoint(&mut self, state: *mut mlua::ffi::lua_State, ticks: u64, phase: u32) {
        let elapsed = self.take(ticks);
        let stack = unsafe { self.stacks.of(state) };
        self.last_stack = Some(stack);
        self.add(phase, stack, elapsed);
        self.add_pc(state, elapsed);
    }

    /// The interpreter was entered from outside (`entered`) or left back to it, and the ticker moved: what ran since the last
    /// booking was the engine's (entered) or the Luau that ran last (left: the thread's frames when it yielded, else the stack the
    /// last safepoint found)
    ///
    /// # Safety
    /// `state` is a live thread of the VM the profile runs on.
    pub unsafe fn vm(&mut self, state: *mut mlua::ffi::lua_State, entered: bool, ticks: u64, phase: u32) {
        let elapsed = self.take(ticks);
        let stack = if entered {
            self.outside_now()
        } else {
            let here = unsafe { self.stacks.of(state) };
            if self.stacks.stacks[here as usize].iter().any(|frame| frame.0 >= 0) { here } else { self.last_stack.unwrap_or(self.outside) }
        };
        self.add(phase, stack, elapsed);
    }

    /// An engine step begins (its name's frame): what ran before it is booked to what was running then
    pub fn step_begin(&mut self, name: &str, in_vm: bool, ticks: u64, phase: u32) {
        let elapsed = self.take(ticks);
        let before = if in_vm { self.last_stack.unwrap_or(self.outside) } else { self.outside_now() };
        self.add(phase, before, elapsed);
        let frame = self.stacks.intern(&[(report::engine_step_function(name), -1)]);
        self.regions.push(frame);
    }

    /// The innermost engine step ends: what ran since the last booking was its
    pub fn step_end(&mut self, ticks: u64, phase: u32) {
        let elapsed = self.take(ticks);
        let frame = self.regions.pop().unwrap_or(self.outside);
        self.add(phase, frame, elapsed);
    }

    /// An Instance API call begins (the ticker moved): what ran before it was the Luau that made it
    ///
    /// # Safety
    /// `state` is a live thread of the VM the profile runs on.
    pub unsafe fn call_begin(&mut self, state: *mut mlua::ffi::lua_State, ticks: u64, phase: u32) {
        let elapsed = self.take(ticks);
        let stack = unsafe { self.stacks.of(state) };
        // (the call's C frame is innermost: the Luau under it ran)
        let frames: Option<Vec<memory::Frame>> = self.stacks.stacks[stack as usize].iter().position(|frame| frame.0 >= 0).map(|index| self.stacks.stacks[stack as usize][index..].to_vec());
        let stack = match frames {
            Some(frames) => self.stacks.intern(&frames),
            None => stack,
        };
        self.last_stack = Some(stack);
        self.add(phase, stack, elapsed);
        self.add_pc(state, elapsed);
    }

    /// An Instance API call ends (the ticker moved during it): what ran since the last booking was the call's, on its line
    ///
    /// # Safety
    /// `state` is a live thread of the VM the profile runs on.
    pub unsafe fn call_end(&mut self, state: *mut mlua::ffi::lua_State, ticks: u64, phase: u32) {
        let elapsed = self.take(ticks);
        let stack = unsafe { self.stacks.of(state) };
        self.add(phase, stack, elapsed);
    }

    /// The profile's end: what ran since the last booking goes where the run was
    pub fn finish(&mut self, ticks: u64, in_vm: bool, phase: u32) {
        let elapsed = self.take(ticks);
        let stack = if in_vm { self.last_stack.unwrap_or(self.outside) } else { self.outside_now() };
        self.add(phase, stack, elapsed);
    }

    /// Milliseconds of the run thread's CPU time in `cycles` (None when the cycles cannot be read)
    pub fn cpu_ms(&self, cycles: u64) -> Option<f64> {
        (self.cycles_per_ms > 0.0).then(|| cycles as f64 / self.cycles_per_ms)
    }
}

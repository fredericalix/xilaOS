//! Tasks of the kernel and their scheduler.
//!
//! A task is a flow of execution with its own stack. The timer interrupt
//! takes the processor from the running task a hundred times per second and
//! gives it to the next task that is ready, in turn. A task that has nothing
//! to do sleeps, and when none is ready the idle task halts the processor
//! until the next interrupt.
//!
//! The state of a task that is not running is on its stack, where the
//! interrupt entry saved it: floating point state, registers, and the frame
//! the processor pushed. Switching is therefore just changing stacks.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::ptr::{addr_of, addr_of_mut};
use core::sync::atomic::{AtomicBool, Ordering};

use xila_core::shell::{TaskInfo, MAX_TASKS};
use xila_core::text::FixedStr;

use crate::arch;

// A query builds its result on the stack: tables of some tens of kilobytes.
const STACK_PAGES: usize = 64;
const STACK_SIZE: usize = STACK_PAGES * xila_core::mem::PAGE;
/// What the interrupt entry leaves on the stack: floating point state,
/// fifteen registers, and the five values pushed by the processor.
const SAVED: usize = 512 + 15 * 8 + 5 * 8;
/// The task that runs when no other can.
const IDLE: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Ready,
    Sleeping,
}

#[derive(Clone, Copy)]
struct Task {
    state: State,
    name: &'static [u8],
    /// Stack pointer of the task while it is not running.
    stack: u64,
    /// Address of the memory of its stack; 0 for the idle task, which keeps
    /// the stack the firmware gave it.
    memory: usize,
    /// Tick at which a sleeping task becomes ready.
    wake: u64,
    /// Ticks during which the task had the processor.
    ran: u64,
    system: bool,
}

impl Task {
    const FREE: Task = Task { state: State::Free, name: b"", stack: 0, memory: 0, wake: 0, ran: 0, system: false };
}

#[repr(C, align(16))]
struct FloatState([u8; 512]);

// Only touched with interrupts off: by `schedule`, which runs inside an
// interrupt, and by the functions below, which turn them off first.
static mut TASKS: [Task; MAX_TASKS] = [Task::FREE; MAX_TASKS];
static mut CURRENT: usize = 0;
/// The floating point state every new task starts with.
static mut FLOAT_START: FloatState = FloatState([0; 512]);

fn tasks() -> &'static mut [Task; MAX_TASKS] {
    unsafe { &mut *addr_of_mut!(TASKS) }
}

fn current() -> usize {
    unsafe { *addr_of!(CURRENT) }
}

/// Makes the flow that is running the idle task. It keeps the stack the
/// firmware gave it, which is small: every task that does work is started
/// with `spawn` and gets a stack of its own.
///
/// # Safety
/// Must be called once, with interrupts off, after `arch::init` and `memory::init`.
pub unsafe fn init() {
    asm!("fxsave64 [{}]", in(reg) addr_of_mut!(FLOAT_START));
    tasks()[IDLE] = Task { state: State::Ready, name: b"idle", stack: 0, memory: 0, wake: 0, ran: 0, system: true };
    *addr_of_mut!(CURRENT) = IDLE;
}

/// What the idle task does: wait for the next interrupt, forever.
pub fn idle() -> ! {
    loop {
        arch::interrupts_on();
        arch::wait();
    }
}

/// Gives a new task a stack and prepares it so that it looks like a task that
/// was interrupted at the first instruction of `entry`. Returns false if
/// there is no memory for the stack.
unsafe fn start(id: usize, name: &'static [u8], entry: extern "C" fn(usize) -> !, argument: usize, system: bool) -> bool {
    let Some(memory) = crate::memory::pages(STACK_PAGES) else {
        return false;
    };
    let top = (memory + STACK_SIZE) as u64;
    let saved = (top - SAVED as u64) as *mut u64;
    core::ptr::copy_nonoverlapping(addr_of!(FLOAT_START) as *const u8, saved as *mut u8, 512);
    // The registers, in the order the interrupt entry pops them: r15 ... rax.
    let registers = saved.add(512 / 8);
    for i in 0..15 {
        registers.add(i).write(0);
    }
    registers.add(12).write(argument as u64); // rcx, the first argument
    let frame = registers.add(15);
    frame.write(entry as *const () as u64); // where to start
    frame.add(1).write(0x08); // code segment
    frame.add(2).write(0x202); // flags: interrupts on
    frame.add(3).write(top - 8); // stack, as if `entry` had just been called
    frame.add(4).write(0x10); // stack segment
    tasks()[id] = Task { state: State::Ready, name, stack: saved as u64, memory, wake: 0, ran: 0, system };
    true
}

/// Starts a task. Returns its number, or `None` if all are in use or there
/// is no memory for its stack.
pub fn spawn(name: &'static [u8], entry: extern "C" fn(usize) -> !, argument: usize, system: bool) -> Option<usize> {
    let on = arch::interrupts_save();
    let id = tasks().iter().position(|task| task.state == State::Free);
    let id = id.filter(|&id| unsafe { start(id, name, entry, argument, system) });
    arch::interrupts_restore(on);
    id
}

/// Stops a task for good. System tasks and the calling task cannot be stopped.
pub fn stop(id: usize) -> bool {
    let on = arch::interrupts_save();
    let task = &mut tasks()[id.min(MAX_TASKS - 1)];
    let stoppable = id < MAX_TASKS && id != current() && task.state != State::Free && !task.system;
    if stoppable {
        // The task is not running, so nothing uses its stack any more.
        crate::memory::free_pages(task.memory, STACK_PAGES);
        *task = Task::FREE;
    }
    arch::interrupts_restore(on);
    stoppable
}

/// Lets the other tasks run for a number of ticks.
pub fn sleep(ticks: u64) {
    let on = arch::interrupts_save();
    let task = &mut tasks()[current()];
    task.state = State::Sleeping;
    task.wake = arch::ticks() + ticks;
    arch::yield_now();
    arch::interrupts_restore(on);
}

/// Describes the tasks for the commands.
pub fn snapshot(out: &mut [TaskInfo; MAX_TASKS]) {
    let on = arch::interrupts_save();
    for (id, task) in tasks().iter().enumerate() {
        out[id] = TaskInfo {
            name: FixedStr::from_bytes(task.name),
            state: match task.state {
                _ if id == current() => b"running",
                State::Ready => b"ready",
                State::Sleeping => b"waiting",
                State::Free => b"",
            },
            ticks: task.ran,
            system: task.system,
            used: task.state != State::Free,
        };
    }
    arch::interrupts_restore(on);
}

/// Chooses the task to run. Called by the interrupt entry with the stack
/// pointer of the task that was running; returns the stack pointer of the
/// task to continue. `tick` is not zero when the timer caused the call.
pub extern "C" fn schedule(stack: u64, tick: u64) -> u64 {
    let tasks = tasks();
    let was = current();
    tasks[was].stack = stack;
    if tick != 0 {
        tasks[was].ran += 1;
    }
    let now = arch::ticks();
    for task in tasks.iter_mut() {
        if task.state == State::Sleeping && now >= task.wake {
            task.state = State::Ready;
        }
    }
    // The next ready task after the one that ran, the idle task if there is none.
    let mut next = IDLE;
    for step in 1..=MAX_TASKS {
        let id = (was + step) % MAX_TASKS;
        if id != IDLE && tasks[id].state == State::Ready {
            next = id;
            break;
        }
    }
    unsafe { *addr_of_mut!(CURRENT) = next };
    tasks[next].stack
}

/// A value shared between tasks: one task at a time may use it.
pub struct Mutex<T> {
    taken: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Mutex<T> {
        Mutex { taken: AtomicBool::new(false), value: UnsafeCell::new(value) }
    }

    /// Waits until no other task uses the value, then takes it.
    pub fn lock(&self) -> Guard<'_, T> {
        while self.taken.swap(true, Ordering::Acquire) {
            arch::yield_now();
        }
        Guard { mutex: self }
    }
}

pub struct Guard<'a, T> {
    mutex: &'a Mutex<T>,
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.mutex.taken.store(false, Ordering::Release);
    }
}

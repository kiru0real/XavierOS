//! Single-core cooperative kernel task scheduler prototype.
//!
//! Tasks have independent static stacks and real saved CPU contexts. They are scheduled only at
//! explicit `yield_now()` points; PIT IRQ0 remains a clock source, not a preemptive scheduler yet.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

const MAX_TASKS: usize = 4;
const STACK_SIZE: usize = 16 * 1024;
const EMPTY: u8 = 0;
const RUNNABLE: u8 = 1;
const RUNNING: u8 = 2;
const FINISHED: u8 = 3;

type TaskEntry = fn();

#[derive(Clone, Copy)]
struct Task {
    id: u32,
    name: &'static str,
    entry: Option<TaskEntry>,
    saved_rsp: u64,
    state: u8,
    switches: u64,
}

impl Task {
    const fn empty() -> Self {
        Self { id: 0, name: "", entry: None, saved_rsp: 0, state: EMPTY, switches: 0 }
    }
}

#[repr(align(16))]
struct TaskStacks([[u8; STACK_SIZE]; MAX_TASKS]);

struct Scheduler {
    tasks: [Task; MAX_TASKS],
    stacks: TaskStacks,
    current: i32,
    host_rsp: u64,
    next_id: u32,
    switches: u64,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: [Task::empty(); MAX_TASKS],
            stacks: TaskStacks([[0; STACK_SIZE]; MAX_TASKS]),
            current: -1,
            host_rsp: 0,
            next_id: 1,
            switches: 0,
        }
    }
}

struct KernelCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for KernelCell<T> {}
impl<T> KernelCell<T> {
    const fn new(value: T) -> Self { Self(UnsafeCell::new(value)) }
    fn get(&self) -> *mut T { self.0.get() }
}

static SCHEDULER: KernelCell<Scheduler> = KernelCell::new(Scheduler::new());
static TEST_COUNTERS: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

unsafe extern "C" {
    fn xavier_context_switch(old_rsp: *mut u64, new_rsp: u64);
}

core::arch::global_asm!(r#"
.intel_syntax noprefix
.section .text
.global xavier_context_switch
.type xavier_context_switch, @function
xavier_context_switch:
    push rbp
    push rbx
    push r12
    push r13
    push r14
    push r15
    mov [rdi], rsp
    mov rsp, rsi
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbx
    pop rbp
    ret
.size xavier_context_switch, .-xavier_context_switch
"#);

pub fn create(name: &'static str, entry: TaskEntry) -> Result<u32, &'static str> {
    let scheduler = unsafe { &mut *SCHEDULER.get() };
    if scheduler.current >= 0 { return Err("CANNOT CREATE TASKS WHILE SCHEDULER IS RUNNING"); }
    let index = scheduler.tasks.iter().position(|task| task.state == EMPTY || task.state == FINISHED)
        .ok_or("TASK TABLE IS FULL")?;
    let id = scheduler.next_id;
    scheduler.next_id = scheduler.next_id.wrapping_add(1).max(1);

    // SysV AMD64 function-entry alignment requires RSP % 16 == 8 after `ret`.
    let stack_start = scheduler.stacks.0[index].as_mut_ptr() as usize;
    let aligned_top = (stack_start + STACK_SIZE) & !0xF;
    let logical_top = aligned_top - 8;
    let initial_rsp = (logical_top - 7 * core::mem::size_of::<u64>()) as *mut u64;
    unsafe {
        initial_rsp.add(0).write(0); // r15
        initial_rsp.add(1).write(0); // r14
        initial_rsp.add(2).write(0); // r13
        initial_rsp.add(3).write(0); // r12
        initial_rsp.add(4).write(0); // rbx
        initial_rsp.add(5).write(0); // rbp
        initial_rsp.add(6).write(task_bootstrap as usize as u64);
    }

    scheduler.tasks[index] = Task { id, name, entry: Some(entry), saved_rsp: initial_rsp as u64, state: RUNNABLE, switches: 0 };
    Ok(id)
}

/// Run all currently runnable tasks until they return. Tasks are cooperative and must yield to let
/// one another run. This call returns to the shell when all scheduled tasks have finished.
pub fn run_until_idle() -> Result<(), &'static str> {
    // Use only raw-pointer accesses across the context switch. A suspended `&mut Scheduler`
    // must not remain live while another task mutates the same scheduler state.
    let scheduler_ptr = SCHEDULER.get();
    unsafe {
        if (*scheduler_ptr).current >= 0 { return Err("SCHEDULER IS ALREADY RUNNING"); }
        let Some(index) = (*scheduler_ptr).tasks.iter().position(|task| task.state == RUNNABLE) else {
            return Err("NO RUNNABLE TASKS");
        };
        (*scheduler_ptr).tasks[index].state = RUNNING;
        (*scheduler_ptr).current = index as i32;
        (*scheduler_ptr).tasks[index].switches = (*scheduler_ptr).tasks[index].switches.saturating_add(1);
        (*scheduler_ptr).switches = (*scheduler_ptr).switches.saturating_add(1);
        let host_rsp_ptr = core::ptr::addr_of_mut!((*scheduler_ptr).host_rsp);
        let task_rsp = (*scheduler_ptr).tasks[index].saved_rsp;
        xavier_context_switch(host_rsp_ptr, task_rsp);
        // Returned only after the last task switched back to this host/kernel stack.
        (*scheduler_ptr).current = -1;
    }
    Ok(())
}

pub fn yield_now() {
    let scheduler_ptr = SCHEDULER.get();
    let current = unsafe { (*scheduler_ptr).current };
    if current < 0 { return; }
    let current_index = current as usize;

    let next = (1..=MAX_TASKS).map(|offset| (current_index + offset) % MAX_TASKS)
        .find(|index| unsafe { *index != current_index && (*scheduler_ptr).tasks[*index].state == RUNNABLE });
    let Some(next_index) = next else { return; };

    unsafe {
        (*scheduler_ptr).tasks[current_index].state = RUNNABLE;
        (*scheduler_ptr).tasks[next_index].state = RUNNING;
        (*scheduler_ptr).tasks[current_index].switches = (*scheduler_ptr).tasks[current_index].switches.saturating_add(1);
        (*scheduler_ptr).tasks[next_index].switches = (*scheduler_ptr).tasks[next_index].switches.saturating_add(1);
        (*scheduler_ptr).switches = (*scheduler_ptr).switches.saturating_add(1);
        (*scheduler_ptr).current = next_index as i32;
        let old_rsp_ptr = core::ptr::addr_of_mut!((*scheduler_ptr).tasks[current_index].saved_rsp);
        let next_rsp = (*scheduler_ptr).tasks[next_index].saved_rsp;
        xavier_context_switch(old_rsp_ptr, next_rsp);
    }
}

extern "C" fn task_bootstrap() -> ! {
    let entry = {
        let scheduler = unsafe { &*SCHEDULER.get() };
        if scheduler.current < 0 { None } else { scheduler.tasks[scheduler.current as usize].entry }
    };
    if let Some(entry) = entry { entry(); }
    finish_current()
}

fn finish_current() -> ! {
    let scheduler_ptr = SCHEDULER.get();
    let current = unsafe { (*scheduler_ptr).current };
    let current_index = if current >= 0 { current as usize } else { MAX_TASKS };
    if current_index >= MAX_TASKS { crate::halt_forever(); }

    unsafe { (*scheduler_ptr).tasks[current_index].state = FINISHED; }
    let next = (1..=MAX_TASKS).map(|offset| (current_index + offset) % MAX_TASKS)
        .find(|index| unsafe { (*scheduler_ptr).tasks[*index].state == RUNNABLE });
    unsafe {
        if let Some(next_index) = next {
            (*scheduler_ptr).tasks[next_index].state = RUNNING;
            (*scheduler_ptr).tasks[next_index].switches = (*scheduler_ptr).tasks[next_index].switches.saturating_add(1);
            (*scheduler_ptr).switches = (*scheduler_ptr).switches.saturating_add(1);
            (*scheduler_ptr).current = next_index as i32;
            let old_rsp_ptr = core::ptr::addr_of_mut!((*scheduler_ptr).tasks[current_index].saved_rsp);
            let next_rsp = (*scheduler_ptr).tasks[next_index].saved_rsp;
            xavier_context_switch(old_rsp_ptr, next_rsp);
        } else {
            (*scheduler_ptr).current = -1;
            let old_rsp_ptr = core::ptr::addr_of_mut!((*scheduler_ptr).tasks[current_index].saved_rsp);
            let host_rsp = (*scheduler_ptr).host_rsp;
            xavier_context_switch(old_rsp_ptr, host_rsp);
        }
    }
    // A finished task must never be resumed.
    crate::halt_forever()
}

pub fn list<F>(mut visit: F)
where F: FnMut(u32, &'static str, &'static str, u64) {
    let scheduler = unsafe { &*SCHEDULER.get() };
    for task in &scheduler.tasks {
        if task.state == EMPTY { continue; }
        let state = match task.state { RUNNABLE => "RUNNABLE", RUNNING => "RUNNING", FINISHED => "FINISHED", _ => "EMPTY" };
        visit(task.id, task.name, state, task.switches);
    }
}

pub fn stats() -> (usize, usize, u64) {
    let scheduler = unsafe { &*SCHEDULER.get() };
    let total = scheduler.tasks.iter().filter(|task| task.state != EMPTY).count();
    let runnable = scheduler.tasks.iter().filter(|task| task.state == RUNNABLE || task.state == RUNNING).count();
    (total, runnable, scheduler.switches)
}

pub fn self_test() -> Result<(), &'static str> {
    if unsafe { (*SCHEDULER.get()).current } >= 0 { return Err("SCHEDULER ALREADY ACTIVE"); }
    for counter in &TEST_COUNTERS { counter.store(0, Ordering::Release); }
    create("test-a", test_task_a)?;
    create("test-b", test_task_b)?;
    create("test-c", test_task_c)?;
    run_until_idle()?;
    if TEST_COUNTERS.iter().any(|counter| counter.load(Ordering::Acquire) != 5) {
        return Err("COOPERATIVE TASK COUNTERS DID NOT COMPLETE");
    }
    let (_, runnable, _) = stats();
    if runnable != 0 { return Err("TASKS REMAIN RUNNABLE AFTER SELF-TEST"); }
    Ok(())
}

fn task_work(counter: usize) {
    for _ in 0..5 {
        TEST_COUNTERS[counter].fetch_add(1, Ordering::AcqRel);
        yield_now();
    }
}
fn test_task_a() { task_work(0); }
fn test_task_b() { task_work(1); }
fn test_task_c() { task_work(2); }

//! Cooperative kernel-thread scheduler foundation.
//!
//! The scheduler now owns dynamic PMM-backed thread stacks, bounded TCB storage, priorities,
//! round-robin selection with priority aging, sleep/wake, blocking, and runtime task creation.
//! It deliberately remains cooperative in this release: PIT ticks wake sleepers but do not
//! preempt a running thread.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use crate::sync::SpinLock;

const MAX_TASKS: usize = 16;
const STACK_PAGES: usize = 4;
const STACK_SIZE: usize = STACK_PAGES * 4096;
const DEFAULT_PRIORITY: u8 = 3;
const MAX_PRIORITY: u8 = 7;
const PRIORITY_AGING_INTERVAL_TICKS: u64 = 25; // 250 ms at the current 100 Hz PIT rate.
const EMPTY: u8 = 0;
const READY: u8 = 1;
const RUNNING: u8 = 2;
const BLOCKED: u8 = 3;
const SLEEPING: u8 = 4;
const ZOMBIE: u8 = 5;
const FINISHED: u8 = 6;

pub type TaskEntry = fn();

#[derive(Clone, Copy)]
struct Task {
    id: u32,
    name: &'static str,
    entry: Option<TaskEntry>,
    saved_rsp: u64,
    stack_physical: u64,
    stack_virtual: u64,
    state: u8,
    priority: u8,
    wake_at: u64,
    ready_since: u64,
    switches: u64,
}

impl Task {
    const fn empty() -> Self {
        Self {
            id: 0, name: "", entry: None, saved_rsp: 0, stack_physical: 0,
            stack_virtual: 0, state: EMPTY, priority: DEFAULT_PRIORITY, wake_at: 0, ready_since: 0, switches: 0,
        }
    }
}

struct Scheduler {
    tasks: [Task; MAX_TASKS],
    current: i32,
    host_rsp: u64,
    next_id: u32,
    last_index: usize,
    switches: u64,
    created: u64,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            tasks: [Task::empty(); MAX_TASKS], current: -1, host_rsp: 0,
            next_id: 1, last_index: MAX_TASKS - 1, switches: 0, created: 0,
        }
    }
}

static SCHEDULER: SpinLock<Scheduler> = SpinLock::new(Scheduler::new());
static TEST_COUNTERS: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
static TEST_SPAWN_FAILED: AtomicBool = AtomicBool::new(false);
static TEST_BLOCKED_ID: AtomicU32 = AtomicU32::new(0);
static TEST_FIRST_TASK: AtomicU32 = AtomicU32::new(0);

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

/// Create a kernel thread with the default priority. Creation is permitted from another
/// running task; the new thread becomes READY and is dispatched at the next scheduling point.
pub fn create(name: &'static str, entry: TaskEntry) -> Result<u32, &'static str> {
    create_with_priority(name, entry, DEFAULT_PRIORITY)
}

/// Create a kernel thread with priority 0 (lowest) through 7 (highest).
pub fn create_with_priority(name: &'static str, entry: TaskEntry, priority: u8) -> Result<u32, &'static str> {
    if priority > MAX_PRIORITY { return Err("TASK PRIORITY MUST BE BETWEEN 0 AND 7"); }
    if !crate::memory::is_initialized() { return Err("PHYSICAL MEMORY MANAGER IS NOT READY"); }

    let mut scheduler = SCHEDULER.lock();
    let index = scheduler.tasks.iter().position(|task| task.state == EMPTY || task.state == FINISHED)
        .ok_or("TASK TABLE IS FULL")?;

    let stack_physical = crate::memory::allocate_contiguous_pages(STACK_PAGES)
        .ok_or("PMM COULD NOT ALLOCATE A CONTIGUOUS TASK STACK")?;
    let stack_virtual = match crate::memory::physical_to_virtual(stack_physical) {
        Some(pointer) => pointer as u64,
        None => {
            let _ = crate::memory::free_contiguous_pages(stack_physical, STACK_PAGES);
            return Err("HHDM COULD NOT ACCESS THE TASK STACK");
        }
    };
    unsafe { core::ptr::write_bytes(stack_virtual as *mut u8, 0, STACK_SIZE); }

    // SysV AMD64 function-entry alignment requires RSP % 16 == 8 after `ret`.
    let aligned_top = ((stack_virtual as usize) + STACK_SIZE) & !0xF;
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

    let id = scheduler.next_id;
    scheduler.next_id = scheduler.next_id.wrapping_add(1).max(1);
    scheduler.tasks[index] = Task {
        id, name, entry: Some(entry), saved_rsp: initial_rsp as u64,
        stack_physical, stack_virtual, state: READY, priority,
        wake_at: 0, ready_since: crate::interrupts::timer_ticks(), switches: 0,
    };
    scheduler.created = scheduler.created.saturating_add(1);
    Ok(id)
}

/// Run READY tasks until they finish or become BLOCKED. Sleeping tasks yield back to the host
/// stack; the host waits for PIT/other IRQ activity and dispatches them once their deadline passes.
pub fn run_until_idle() -> Result<(), &'static str> {
    {
        let scheduler = SCHEDULER.lock();
        if scheduler.current >= 0 { return Err("SCHEDULER IS ALREADY RUNNING"); }
    }

    loop {
        enum Action { Dispatch(*mut u64, u64), Wait, Done }
        let action = {
            let mut scheduler = SCHEDULER.lock();
            wake_expired(&mut scheduler, crate::interrupts::timer_ticks());
            if let Some(index) = choose_ready(&scheduler, None) {
                scheduler.tasks[index].state = RUNNING;
                scheduler.tasks[index].switches = scheduler.tasks[index].switches.saturating_add(1);
                scheduler.switches = scheduler.switches.saturating_add(1);
                scheduler.current = index as i32;
                scheduler.last_index = index;
                let host_rsp = core::ptr::addr_of_mut!(scheduler.host_rsp);
                let task_rsp = scheduler.tasks[index].saved_rsp;
                Action::Dispatch(host_rsp, task_rsp)
            } else if scheduler.tasks.iter().any(|task| task.state == SLEEPING) {
                Action::Wait
            } else {
                Action::Done
            }
        };

        match action {
            Action::Dispatch(host_rsp, task_rsp) => {
                unsafe { xavier_context_switch(host_rsp, task_rsp); }
                // Control returns here only after a task has yielded to the host stack.
                {
                    let mut scheduler = SCHEDULER.lock();
                    scheduler.current = -1;
                }
                reap_zombies();
            }
            Action::Wait => crate::interrupts::wait_for_interrupt(),
            Action::Done => {
                reap_zombies();
                return Ok(());
            }
        }
    }
}

/// Cooperatively yield the CPU to another READY thread.
pub fn yield_now() {
    let current = {
        let mut scheduler = SCHEDULER.lock();
        if scheduler.current < 0 { return; }
        let index = scheduler.current as usize;
        scheduler.tasks[index].state = READY;
        scheduler.tasks[index].ready_since = crate::interrupts::timer_ticks();
        index
    };
    switch_from_current(current);
}

/// Sleep for at least `ticks` PIT ticks. This is cooperative; the timer wakes the task but does
/// not interrupt a different RUNNING task.
pub fn sleep_ticks(ticks: u64) {
    if ticks == 0 { yield_now(); return; }
    let current = {
        let mut scheduler = SCHEDULER.lock();
        if scheduler.current < 0 { return; }
        let index = scheduler.current as usize;
        scheduler.tasks[index].wake_at = crate::interrupts::timer_ticks().wrapping_add(ticks);
        scheduler.tasks[index].state = SLEEPING;
        index
    };
    switch_from_current(current);
}

/// Mark the current thread BLOCKED until wake(task_id) is called.
pub fn block_current() {
    let current = {
        let mut scheduler = SCHEDULER.lock();
        if scheduler.current < 0 { return; }
        let index = scheduler.current as usize;
        scheduler.tasks[index].state = BLOCKED;
        index
    };
    switch_from_current(current);
}

/// Move a BLOCKED or SLEEPING thread to READY. Returns false if the ID is unknown or not waitable.
pub fn wake(task_id: u32) -> bool {
    let mut scheduler = SCHEDULER.lock();
    let Some(task) = scheduler.tasks.iter_mut().find(|task| task.id == task_id) else { return false; };
    if task.state != BLOCKED && task.state != SLEEPING { return false; }
    task.state = READY;
    task.wake_at = 0;
    task.ready_since = crate::interrupts::timer_ticks();
    true
}

fn switch_from_current(current_index: usize) {
    let dispatch = {
        let mut scheduler = SCHEDULER.lock();
        wake_expired(&mut scheduler, crate::interrupts::timer_ticks());
        let old_rsp = core::ptr::addr_of_mut!(scheduler.tasks[current_index].saved_rsp);
        if let Some(next_index) = choose_ready(&scheduler, Some(current_index)) {
            if next_index == current_index {
                scheduler.tasks[current_index].state = RUNNING;
                return;
            }
            scheduler.tasks[current_index].switches = scheduler.tasks[current_index].switches.saturating_add(1);
            scheduler.tasks[next_index].state = RUNNING;
            scheduler.tasks[next_index].switches = scheduler.tasks[next_index].switches.saturating_add(1);
            scheduler.switches = scheduler.switches.saturating_add(1);
            scheduler.current = next_index as i32;
            scheduler.last_index = next_index;
            let next_rsp = scheduler.tasks[next_index].saved_rsp;
            Some((old_rsp, next_rsp))
        } else {
            scheduler.current = -1;
            Some((old_rsp, scheduler.host_rsp))
        }
    };

    if let Some((old_rsp, next_rsp)) = dispatch {
        unsafe { xavier_context_switch(old_rsp, next_rsp); }
    }
}

fn choose_ready(scheduler: &Scheduler, after: Option<usize>) -> Option<usize> {
    let start = after.unwrap_or(scheduler.last_index);
    let now = crate::interrupts::timer_ticks();
    let mut best: Option<usize> = None;
    for offset in 1..=MAX_TASKS {
        let index = (start + offset) % MAX_TASKS;
        let task = scheduler.tasks[index];
        if task.state != READY { continue; }
        match best {
            None => best = Some(index),
            Some(current_best) if effective_priority(&task, now) > effective_priority(&scheduler.tasks[current_best], now) => best = Some(index),
            _ => {}
        }
    }
    best
}

fn effective_priority(task: &Task, now: u64) -> u8 {
    let waited = now.wrapping_sub(task.ready_since);
    if waited >= (1u64 << 63) { return task.priority; }
    let bonus = (waited / PRIORITY_AGING_INTERVAL_TICKS).min(MAX_PRIORITY as u64) as u8;
    task.priority.saturating_add(bonus).min(MAX_PRIORITY)
}

fn wake_expired(scheduler: &mut Scheduler, now: u64) {
    for task in &mut scheduler.tasks {
        if task.state == SLEEPING && now.wrapping_sub(task.wake_at) < (1u64 << 63) {
            task.state = READY;
            task.wake_at = 0;
            task.ready_since = now;
        }
    }
}

extern "C" fn task_bootstrap() -> ! {
    let entry = {
        let scheduler = SCHEDULER.lock();
        if scheduler.current < 0 { None } else { scheduler.tasks[scheduler.current as usize].entry }
    };
    if let Some(entry) = entry { entry(); }
    finish_current()
}

fn finish_current() -> ! {
    let current = {
        let mut scheduler = SCHEDULER.lock();
        if scheduler.current < 0 { crate::halt_forever(); }
        let index = scheduler.current as usize;
        scheduler.tasks[index].state = ZOMBIE;
        index
    };
    switch_from_current(current);
    // A ZOMBIE context must never be resumed. Its stack is released from the host context.
    crate::halt_forever()
}

fn reap_zombies() {
    let mut pending = [(0usize, 0u64); MAX_TASKS];
    let count = {
        let scheduler = SCHEDULER.lock();
        let mut count = 0usize;
        for (index, task) in scheduler.tasks.iter().enumerate() {
            if task.state == ZOMBIE && task.stack_physical != 0 {
                pending[count] = (index, task.stack_physical);
                count += 1;
            }
        }
        count
    };

    for &(index, physical) in &pending[..count] {
        if !crate::memory::free_contiguous_pages(physical, STACK_PAGES) {
            crate::klog::log(crate::klog::Level::Error, "scheduler could not release a finished task stack");
            continue;
        }
        let mut scheduler = SCHEDULER.lock();
        let task = &mut scheduler.tasks[index];
        if task.state == ZOMBIE && task.stack_physical == physical {
            task.stack_physical = 0;
            task.stack_virtual = 0;
            task.saved_rsp = 0;
            task.entry = None;
            task.state = FINISHED;
        }
    }
}

#[derive(Clone, Copy)]
pub struct SchedulerStats {
    pub total: usize,
    pub ready: usize,
    pub running: usize,
    pub blocked: usize,
    pub sleeping: usize,
    pub zombie: usize,
    pub finished: usize,
    pub switches: u64,
    pub created: u64,
    pub current_id: u32,
}

pub fn detailed_stats() -> SchedulerStats {
    let scheduler = SCHEDULER.lock();
    let mut result = SchedulerStats {
        total: 0, ready: 0, running: 0, blocked: 0, sleeping: 0, zombie: 0,
        finished: 0, switches: scheduler.switches, created: scheduler.created,
        current_id: 0,
    };
    if scheduler.current >= 0 { result.current_id = scheduler.tasks[scheduler.current as usize].id; }
    for task in &scheduler.tasks {
        if task.state == EMPTY { continue; }
        result.total += 1;
        match task.state {
            READY => result.ready += 1,
            RUNNING => result.running += 1,
            BLOCKED => result.blocked += 1,
            SLEEPING => result.sleeping += 1,
            ZOMBIE => result.zombie += 1,
            FINISHED => result.finished += 1,
            _ => {}
        }
    }
    result
}

pub fn list<F>(mut visit: F)
where F: FnMut(u32, &'static str, &'static str, u8, u8, u64) {
    let tasks = {
        let scheduler = SCHEDULER.lock();
        scheduler.tasks
    };
    let now = crate::interrupts::timer_ticks();
    for task in &tasks {
        if task.state == EMPTY { continue; }
        let state = match task.state {
            READY => "READY", RUNNING => "RUNNING", BLOCKED => "BLOCKED",
            SLEEPING => "SLEEPING", ZOMBIE => "ZOMBIE", FINISHED => "FINISHED", _ => "EMPTY",
        };
        visit(task.id, task.name, state, task.priority, effective_priority(task, now), task.switches);
    }
}

/// Compatibility tuple used by existing desktop and diagnostics views.
pub fn stats() -> (usize, usize, u64) {
    let stats = detailed_stats();
    (stats.total, stats.ready + stats.running, stats.switches)
}

pub fn self_test() -> Result<(), &'static str> {
    {
        let scheduler = SCHEDULER.lock();
        if scheduler.current >= 0 { return Err("SCHEDULER ALREADY ACTIVE"); }
        if scheduler.tasks.iter().any(|task| matches!(task.state, READY | RUNNING | BLOCKED | SLEEPING | ZOMBIE)) {
            return Err("SCHEDULER SELF-TEST REQUIRES NO ACTIVE TASKS");
        }
    }
    for counter in &TEST_COUNTERS { counter.store(0, Ordering::Release); }
    TEST_SPAWN_FAILED.store(false, Ordering::Release);
    TEST_BLOCKED_ID.store(0, Ordering::Release);
    TEST_FIRST_TASK.store(0, Ordering::Release);
    let free_before = crate::memory::free_page_count();
    create_with_priority("test-a", test_task_a, 3)?;
    create_with_priority("test-b", test_task_b, 3)?;
    create_with_priority("test-c", test_task_c, 3)?;
    run_until_idle()?;

    if TEST_SPAWN_FAILED.load(Ordering::Acquire) { return Err("RUNTIME TASK CREATION OR BLOCK/WAKE FAILED"); }
    if TEST_FIRST_TASK.load(Ordering::Acquire) != 4 { return Err("PRIORITY SCHEDULING DID NOT SELECT THE HIGHER-PRIORITY TASK FIRST"); }
    if TEST_COUNTERS.iter().any(|counter| counter.load(Ordering::Acquire) != 5) {
        return Err("TASK COUNTERS DID NOT COMPLETE");
    }
    let after = crate::memory::free_page_count();
    if free_before != after { return Err("TASK STACK PAGES WERE NOT RECLAIMED"); }
    let stats = detailed_stats();
    if stats.ready + stats.running + stats.blocked + stats.sleeping + stats.zombie != 0 {
        return Err("TASKS REMAIN ACTIVE OR BLOCKED AFTER SELF-TEST");
    }
    if effective_priority(&Task { priority: 0, ready_since: 0, ..Task::empty() }, PRIORITY_AGING_INTERVAL_TICKS * 7) != MAX_PRIORITY {
        return Err("PRIORITY AGING DID NOT PROMOTE A LONG-WAITING TASK");
    }
    Ok(())
}

fn task_work(counter: usize) {
    for _ in 0..5 {
        TEST_COUNTERS[counter].fetch_add(1, Ordering::AcqRel);
        yield_now();
    }
}

fn test_task_a() {
    let spawned = create_with_priority("test-d", test_task_d, 4);
    let blocked = create_with_priority("test-blocked", test_task_blocked, 6);
    let waker = create_with_priority("test-waker", test_task_waker, 1);
    match (spawned, blocked, waker) {
        (Ok(_), Ok(blocked_id), Ok(_)) => TEST_BLOCKED_ID.store(blocked_id, Ordering::Release),
        _ => TEST_SPAWN_FAILED.store(true, Ordering::Release),
    }
    task_work(0);
}
fn test_task_b() {
    let _ = TEST_FIRST_TASK.compare_exchange(0, 2, Ordering::AcqRel, Ordering::Relaxed);
    task_work(1);
}
fn test_task_c() {
    let _ = TEST_FIRST_TASK.compare_exchange(0, 3, Ordering::AcqRel, Ordering::Relaxed);
    task_work(2);
}
fn test_task_d() {
    let _ = TEST_FIRST_TASK.compare_exchange(0, 4, Ordering::AcqRel, Ordering::Relaxed);
    sleep_ticks(2);
    task_work(3);
}
fn test_task_blocked() {
    block_current();
    task_work(4);
}
fn test_task_waker() {
    let blocked_id = TEST_BLOCKED_ID.load(Ordering::Acquire);
    if blocked_id == 0 || !wake(blocked_id) {
        TEST_SPAWN_FAILED.store(true, Ordering::Release);
    }
}

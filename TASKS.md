# XavierOS cooperative tasks

The current development snapshot contains a small single-core cooperative scheduler prototype in `src/task.rs`.

## Design

- Up to four task records, each with a separate 16 KiB statically allocated stack.
- x86_64 assembly saves/restores the callee-saved registers and stack pointer at a context switch.
- `task::yield_now()` chooses another task in `RUNNABLE` state. Tasks that do not yield can monopolize the CPU.
- `task::run_until_idle()` starts the first runnable task and returns to the shell after every created task exits.
- The `TASKTEST` command runs three small tasks that each update a counter and yield; `SELFTEST` includes the same check.

## Limitations and safety constraints

This is not preemptive multitasking. PIT IRQ0 still increments the uptime counter only; the IRQ handler does not change task context. There is no task blocking/sleep queue, idle task, process isolation, user mode, address-space switching, FPU/SIMD state management, or SMP support.

The scheduler and PMM/heap are currently designed for one CPU. Do not yield while holding a spin lock or shared kernel resource. The task stacks are static and fixed-size. A real process scheduler should wait until exception/interrupt entry, stack ownership, page tables, syscalls, task termination, and resource cleanup are better structured.

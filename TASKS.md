# Cooperative Tasks

The current task subsystem is a small single-core cooperative scheduler prototype.

## Design

- Up to four task records with separate fixed-size static stacks.
- x86_64 assembly saves/restores the callee-saved context and stack pointer.
- `task::yield_now()` chooses another runnable task.
- Test tasks increment counters and yield; `TASKTEST` and `SELFTEST` exercise this path.

## Limitations

This is not preemptive multitasking. PIT IRQ0 updates uptime but does not switch tasks. There is no task blocking/sleep queue, process isolation, user mode, address-space switching, FPU/SIMD state management, or SMP support.

Tasks that do not yield can monopolize the CPU. Do not yield while holding a shared kernel resource. A production scheduler needs a carefully specified interrupt frame, stack ownership, page-table switching, syscalls, cleanup, and synchronization policy.

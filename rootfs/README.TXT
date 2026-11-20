XAVIEROS 0.2.5

The desktop boots to a framebuffer wallpaper, dock, and custom transparent mouse cursor.
Use the mouse if available, or navigate the dock with Left/Right and Enter.
Press T to open Terminal. Type DESKTOP in Terminal to return here.

The Files, Network, Clock, Settings and About windows are preview panels
inside the kernel, not isolated user applications.

Version: 0.2.5
License: MIT

Kernel foundations: IRQ-safe heap/PMM locks; cooperative kernel threads with
dynamic stacks, priorities, sleep/wake; active four-level supervisor VMM;
fixed-capacity kernel event log and scheduler/memory diagnostics.
Limitations: no preemption, Ring 3 processes, SMP, or isolated address spaces.

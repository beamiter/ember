# Scoped native publication feedback

Vendored from crates.io `wgpu-hal 30.0.1`; upstream licenses are retained.
Only Vulkan feature discovery/device enabling, swapchain present IDs and a
synchronous, thread-local observation scope are extended. The scope must enclose
exactly one native present. An exact `vkWaitForPresentKHR` success can advance an
X11 extended frame counter. A compositor scene acknowledgement is still required.
No queue/fence/timeout fallback fabricates presentation. Other backends and
drivers lacking the extensions return unknown. The optional bounded wait runs
only for requested evidence; latency/hardware coverage remain acceptance work.

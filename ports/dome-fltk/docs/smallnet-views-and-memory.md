# Smallnet view opening and native heap reclamation

Smallnet links selected through **Open Link in New View** now use the same
unrelated top-level view builder as the browser's New Tab command. The source
view remains open, and closing the new view returns to it. The destination
loads with its own document policy and has no opener relationship.

The converted smallnet document retains its inert Content Security Policy and
sandbox. Smallnet's native transport handlers do not require JavaScript, DOM
storage, embedded frames or page-created popups. Removing document restrictions
would not improve allocator retention or swap recovery. Browser menu commands
are implemented outside the document's popup-creation path; they do not require
adding `allow-popups` to a remote document. Private, view-bound input actions
continue to belong to their source view. Smallnet remains outside web adblocking.

This is a document policy, not a claim of operating-system process isolation.
The [HTML sandbox model](https://html.spec.whatwg.org/multipage/browsers.html#sandboxing)
explains the distinction between auxiliary browsing contexts created by content
and the opaque document origin.

On Linux/glibc, startup limits native allocator arenas to eight unless the
launcher explicitly configures `MALLOC_ARENA_MAX` or
`glibc.malloc.arena_max` in `GLIBC_TUNABLES`. A background worker checks native
free blocks at most once every 30 seconds and calls `malloc_trim` only with
at least 32 MiB of allocator-owned free space. It passes 128 KiB of padding to the trim call for future allocations. The worker holds no browser/engine objects,
has a bounded request queue, and wakes to exit when its sender is dropped.

This returns unused native pages to the operating system. It does not discard
live tabs, trigger a forced JavaScript collection, change GC safety settings,
modify global VM parameters or move live data into swap. JavaScript and images
still held by live documents remain allocated. The [glibc allocator tunables](https://sourceware.org/glibc/manual/latest/html_node/Memory-Allocation-Tunables.html)
describe the arena controls and the distinction between mapped allocations and
allocator-retained pages.

For a local comparison, `DOME_BROWSER_MEMORY_RECLAIM=0` disables this browser
policy. `DOME_BROWSER_MEMORY_TRACE=1` records aggregate allocator sizes and purge
durations without URLs, page text or profile data. A purge lasting at least
250 ms is logged even without the trace flag.

Validation results are recorded separately; these mechanisms alone are not a
claim that Discord's reported periodic freezes are fully resolved.

Validation of the optimized release passes 211 frontend unit tests and 116
runtime checks, plus three worker-shutdown scenarios. Native view opening
passes 11 checks each on Wayland and X11; the installed predecessor selects the
same menu action but cannot retrieve the new destination.

The same executable and allocation/release fixture settle at 687.0 MiB with
the memory policy disabled and 466.4 MiB enabled: a 32.1% reduction in resident
memory plus swap. Both private runs use zero swap; this is a measured reduction
in retained memory, not a direct swap-latency result. The enabled run preserves
live data, keeps native UI pings below 5.3 ms, and records a maximum animation
gap of 124 ms. Its native purges take 10 ms and less than 1 ms. These values
apply to this bounded local workload, without induced system memory pressure.

The anonymous live Discord login runs for 90 seconds and closes cleanly.
Authenticated, long-session periodic freezing remains unconfirmed. See
[the validation record](validation-smallnet-memory-2026-10-29.json) for scope,
source hashes, controls and regression results.

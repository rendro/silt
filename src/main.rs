//! `silt` — the command. Everything it does is the library's
//! `silt::cli::main` (src/cli/mod.rs); the binary adds the allocator.

// The silt binary allocates with mimalloc (decided 2026-10-17; the
// library, embedders and the wasm playground keep their own allocator).
// With glibc, tasks that free memory another worker's arena handed out
// contend for that arena's lock: four tasks that each make a small list
// per round ran two to three times slower than one in most runs
// (`tlist` 14 of 14), against 5 of 21 on mimalloc, whose heaps take a
// free from another thread without a lock. Code that allocates also
// runs 4-17% faster on one task.
#[cfg(not(target_arch = "wasm32"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// mimalloc's `arena_eager_commit` option (its index in `mi_option_e`,
/// the same in mimalloc v2 and v3; the Rust bindings have no name for
/// it). By default mimalloc commits whole arena segments up front on
/// systems that overcommit, which is Linux: 9 MB more for every program
/// than glibc, and 15-19 MB more with a few tasks. Off, a program starts
/// within 0.3 MB of glibc and runs as fast.
#[cfg(target_os = "linux")]
const ARENA_EAGER_COMMIT: libmimalloc_sys::mi_option_t = 4;

/// Turns eager commit off before the runtime's first allocation reserves
/// an arena: from `main` it is too late. It runs as a constructor, after
/// mimalloc's own (priority 101) and before `main`.
#[cfg(target_os = "linux")]
#[used]
#[unsafe(link_section = ".init_array")]
static LAZY_COMMIT: extern "C" fn() = {
    extern "C" fn lazy_commit() {
        // SAFETY: sets one of mimalloc's options; no memory is touched.
        unsafe { libmimalloc_sys::mi_option_set(ARENA_EAGER_COMMIT, 0) }
    }
    lazy_commit
};

fn main() {
    silt::cli::main()
}

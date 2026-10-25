//! Per-thread scheduling for background index work inside daemons.
//! Linux applies nice and I/O priority per thread, so request workers keep
//! normal priority while maintenance and preparation threads yield.
//! Contract: `docs/router-lifecycle.md#process-placement`.

/// Nice value of background index threads; request threads stay at the process value.
pub const BACKGROUND_NICE: i32 = 10;
/// `IOPRIO_CLASS_IDLE` in the class bits of an I/O priority.
const IDLE_IO_PRIORITY: libc::c_long = 3 << 13;
/// `IOPRIO_WHO_PROCESS`; with id 0 it names the calling thread.
const IOPRIO_WHO_THREAD: libc::c_long = 1;

/// Lowers the calling thread, and threads it spawns later, to
/// [`BACKGROUND_NICE`] and idle I/O. Failures leave the priority unchanged.
pub fn lower_current_thread() {
    // SAFETY: plain syscalls on the calling thread with constant arguments.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, BACKGROUND_NICE);
        libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_THREAD, 0, IDLE_IO_PRIORITY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_nice() -> i32 {
        // SAFETY: reads the calling thread's nice value.
        unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }
    }

    fn thread_io_priority() -> libc::c_long {
        // SAFETY: reads the calling thread's I/O priority.
        unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_THREAD, 0) }
    }

    #[test]
    fn lowering_applies_to_the_calling_thread_and_its_children_only() {
        let caller_nice = thread_nice();
        let (lowered, child) = std::thread::spawn(|| {
            lower_current_thread();
            let child = std::thread::spawn(|| (thread_nice(), thread_io_priority()))
                .join()
                .unwrap();
            ((thread_nice(), thread_io_priority()), child)
        })
        .join()
        .unwrap();
        for (nice, io) in [lowered, child] {
            assert!(nice >= BACKGROUND_NICE.max(caller_nice), "nice {nice}");
            assert_eq!(io >> 13, 3, "idle I/O class, got {io:#x}");
        }
        assert_eq!(thread_nice(), caller_nice, "the caller keeps its priority");
        assert_ne!(thread_io_priority() >> 13, 3);
    }
}

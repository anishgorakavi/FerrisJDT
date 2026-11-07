//! Resident set size, read from the operating system.
//!
//! Resident memory is the only figure worth quoting: virtual size counts
//! address space that was never touched, and an allocator's own accounting
//! misses the pages behind memory-mapped files and the runtime itself.

/// Current resident set size in bytes, or `None` where it cannot be read.
pub fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // /proc/self/statm reports pages; field 2 is the resident set.
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let page_size = unsafe { libc_sysconf_pagesize() };
        Some(pages * page_size)
    }
    #[cfg(target_os = "macos")]
    {
        // `ps` is used rather than task_info to avoid a libc dependency and
        // any unsafe code for what is a diagnostic path.
        let output = std::process::Command::new("/bin/ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kilobytes: u64 = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .ok()?;
        Some(kilobytes * 1024)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
unsafe fn libc_sysconf_pagesize() -> u64 {
    extern "C" {
        fn sysconf(name: i32) -> i64;
    }
    // _SC_PAGESIZE is 30 on Linux.
    let value = sysconf(30);
    if value > 0 {
        value as u64
    } else {
        4096
    }
}

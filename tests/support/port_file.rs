//! How a test learns the port of a listener that a silt program bound.
//!
//! The program listens on port 0, so that the system chooses a free
//! port, and writes the port into a file the test named. The test
//! waits for the file. (The other way round, a port chosen by the
//! test by binding and dropping a listener, is free only until some
//! other process takes it.)

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A file for one silt program to write its port into.
pub struct PortFile(PathBuf);

impl PortFile {
    #[allow(clippy::new_without_default)]
    pub fn new() -> PortFile {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("silt_port_{}_{n}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        PortFile(path)
    }

    /// The path, as a silt string literal can hold it on every
    /// platform.
    pub fn path(&self) -> String {
        self.0.to_string_lossy().replace('\\', "/")
    }

    /// The port, once the program has written it: the listener is
    /// bound then, and a connection to it is queued until it is
    /// accepted. Panics if nothing comes within 30 seconds.
    pub fn wait(&self) -> u16 {
        let limit = Instant::now() + Duration::from_secs(30);
        loop {
            // The line is complete when its newline is there.
            if let Ok(text) = std::fs::read_to_string(&self.0)
                && let Some(line) = text.strip_suffix('\n')
                && let Ok(port) = line.trim().parse()
            {
                return port;
            }
            assert!(
                Instant::now() < limit,
                "the silt program did not write its port to {}",
                self.0.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for PortFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

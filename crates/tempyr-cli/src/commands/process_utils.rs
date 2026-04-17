use std::thread;
use std::time::{Duration, Instant};

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn wait_for_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        thread::sleep(PROCESS_POLL_INTERVAL.min(deadline - now));
    }
}

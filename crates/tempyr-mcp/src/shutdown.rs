use std::fmt;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Result, anyhow, bail};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum ShutdownReason {
    StdinEof,
    ParentExited { parent_pid: u32 },
}

impl fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StdinEof => write!(f, "tempyr shutting down: stdin EOF"),
            Self::ParentExited { parent_pid } => {
                write!(f, "tempyr shutting down: parent pid {parent_pid} exited")
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct ShutdownCoordinator {
    cancellation_token: CancellationToken,
    reason: Arc<Mutex<Option<ShutdownReason>>>,
}

impl ShutdownCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation_token.clone()
    }

    pub fn spawn_parent_watcher(&self) {
        if let Err(err) = spawn_parent_watcher(self.reason.clone(), self.cancellation_token()) {
            eprintln!("Warning: tempyr could not watch parent process: {err}");
        }
    }

    pub fn graceful_exit(&self, fallback: ShutdownReason) -> Result<()> {
        let reason = self.record_if_absent(fallback);
        eprintln!("{reason}");
        Ok(())
    }

    pub fn graceful_cancelled(&self) -> Result<()> {
        let reason = self
            .current_reason()
            .ok_or_else(|| anyhow!("MCP service cancelled without a shutdown reason"))?;
        eprintln!("{reason}");
        Ok(())
    }

    fn current_reason(&self) -> Option<ShutdownReason> {
        self.reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record_if_absent(&self, fallback: ShutdownReason) -> ShutdownReason {
        let mut reason = self
            .reason
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let selected = reason.get_or_insert(fallback);
        selected.clone()
    }
}

fn spawn_parent_watcher(
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    cancellation_token: CancellationToken,
) -> Result<()> {
    #[cfg(unix)]
    {
        spawn_unix_parent_watcher(reason, cancellation_token)
    }
    #[cfg(windows)]
    {
        spawn_windows_parent_watcher(reason, cancellation_token)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = reason;
        let _ = cancellation_token;
        bail!("parent watcher is not implemented for this platform")
    }
}

fn trigger_parent_exit(
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    cancellation_token: CancellationToken,
    parent_pid: u32,
) {
    let mut slot = reason
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if slot.is_none() {
        slot.replace(ShutdownReason::ParentExited { parent_pid });
    }
    drop(slot);
    cancellation_token.cancel();
}

#[cfg(unix)]
fn spawn_unix_parent_watcher(
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    cancellation_token: CancellationToken,
) -> Result<()> {
    let original_parent_pid = unsafe { libc::getppid() };
    let parent_pid = u32::try_from(original_parent_pid)
        .map_err(|_| anyhow!("invalid parent pid reported by getppid(): {original_parent_pid}"))?;

    thread::Builder::new()
        .name("tempyr-parent-watch".to_string())
        .spawn(move || {
            loop {
                thread::sleep(std::time::Duration::from_secs(5));
                let current_parent_pid = unsafe { libc::getppid() };
                if current_parent_pid != original_parent_pid {
                    trigger_parent_exit(reason, cancellation_token, parent_pid);
                    return;
                }
            }
        })
        .map(|_| ())
        .map_err(Into::into)
}

#[cfg(windows)]
fn spawn_windows_parent_watcher(
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    cancellation_token: CancellationToken,
) -> Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let parent_pid = unsafe { get_parent_pid()? };
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, parent_pid) };
    if handle.is_null() {
        if !unsafe { process_exists(parent_pid)? } {
            trigger_parent_exit(reason, cancellation_token, parent_pid);
            return Ok(());
        }
        bail!(
            "OpenProcess failed for parent pid {parent_pid}: {}",
            std::io::Error::last_os_error()
        );
    }
    let handle_value = handle as usize;

    thread::Builder::new()
        .name("tempyr-parent-watch".to_string())
        .spawn(move || unsafe {
            let handle = handle_value as windows_sys::Win32::Foundation::HANDLE;
            let wait_result = WaitForSingleObject(handle, INFINITE);
            let close_result = CloseHandle(handle);
            if close_result == 0 {
                eprintln!(
                    "Warning: tempyr could not close parent watcher handle: {}",
                    std::io::Error::last_os_error()
                );
            }

            match wait_result {
                WAIT_OBJECT_0 => trigger_parent_exit(reason, cancellation_token, parent_pid),
                WAIT_FAILED => eprintln!(
                    "Warning: tempyr parent watcher wait failed: {}",
                    std::io::Error::last_os_error()
                ),
                other => {
                    eprintln!("Warning: tempyr parent watcher returned unexpected status {other}")
                }
            }
        })
        .map(|_| ())
        .map_err(|err| {
            unsafe {
                let _ = CloseHandle(handle);
            }
            err.into()
        })
}

#[cfg(windows)]
unsafe fn get_parent_pid() -> Result<u32> {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    let current_pid = unsafe { GetCurrentProcessId() };
    let found_parent = unsafe {
        find_process_entry(
            |entry| entry.th32ProcessID == current_pid,
            || "CreateToolhelp32Snapshot failed".to_string(),
            || "CloseHandle failed for process snapshot".to_string(),
        )?
    }
    .map(|entry| entry.th32ParentProcessID);

    found_parent.ok_or_else(|| anyhow!("could not resolve parent pid for process {current_pid}"))
}

#[cfg(windows)]
unsafe fn process_exists(target_pid: u32) -> Result<bool> {
    Ok(unsafe {
        find_process_entry(
            |entry| entry.th32ProcessID == target_pid,
            || format!("CreateToolhelp32Snapshot failed while checking pid {target_pid}"),
            || format!("CloseHandle failed for process snapshot while checking pid {target_pid}"),
        )?
    }
    .is_some())
}

#[cfg(windows)]
unsafe fn find_process_entry(
    mut predicate: impl FnMut(
        &windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W,
    ) -> bool,
    snapshot_error: impl FnOnce() -> String,
    close_error: impl FnOnce() -> String,
) -> Result<Option<windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W>> {
    use std::mem::size_of;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        bail!("{}: {}", snapshot_error(), std::io::Error::last_os_error());
    }

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = None;
    let mut enumeration_error = None;

    if unsafe { Process32FirstW(snapshot, &mut entry) } != 0 {
        loop {
            if predicate(&entry) {
                found = Some(entry);
                break;
            }
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                let err = unsafe { GetLastError() };
                if err != ERROR_NO_MORE_FILES {
                    enumeration_error = Some(anyhow!(
                        "Process32NextW failed while enumerating process snapshot: {}",
                        std::io::Error::from_raw_os_error(err as i32)
                    ));
                }
                break;
            }
        }
    } else {
        let err = unsafe { GetLastError() };
        if err != ERROR_NO_MORE_FILES {
            enumeration_error = Some(anyhow!(
                "Process32FirstW failed while enumerating process snapshot: {}",
                std::io::Error::from_raw_os_error(err as i32)
            ));
        }
    }

    let close_handle_error = if unsafe { CloseHandle(snapshot) } == 0 {
        Some(std::io::Error::last_os_error())
    } else {
        None
    };

    if let Some(err) = enumeration_error {
        return Err(err);
    }

    if let Some(err) = close_handle_error {
        bail!("{}: {}", close_error(), err);
    }

    Ok(found)
}

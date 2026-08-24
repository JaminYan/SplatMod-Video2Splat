use std::{
    ffi::OsString,
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Mutex,
};

use crate::error::{Result, SplatError};

#[derive(Debug, Clone, Copy)]
pub enum ProcessStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone)]
pub enum ProcessUpdate {
    Started { pid: Option<u32> },
    Line { stream: ProcessStream, line: String },
    Heartbeat { elapsed_ms: u64 },
}

pub type ProcessObserver = Arc<dyn Fn(ProcessUpdate) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct ProcessSpec {
    pub executable: PathBuf,
    pub args: Vec<OsString>,
    pub working_directory: Option<PathBuf>,
    pub log_path: Option<PathBuf>,
    pub observer: Option<ProcessObserver>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    /// 进程是否因用户取消而被终止。
    pub cancelled: bool,
}

#[derive(Debug, Default)]
pub struct ProcessManager {
    cancel_flag: Arc<AtomicBool>,
    child: Mutex<Option<tokio::process::Child>>,
}

type SharedFile = Arc<Mutex<tokio::fs::File>>;

/// 挂起直到 cancel_flag 被置位，用于在等待子进程时轮询取消信号。
async fn wait_until_cancelled(flag: &Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    pub async fn run(&self, spec: ProcessSpec) -> Result<ProcessOutput> {
        // A PipelineRunner owns one ProcessManager for its entire task. Never
        // clear a user cancellation between sequential FFprobe/FFmpeg batches:
        // doing so made Stop appear to work while the next batch restarted.
        if self.cancel_flag.load(Ordering::SeqCst) {
            return Err(SplatError::Cancelled);
        }
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = spec.working_directory.as_ref() {
            command.current_dir(dir);
        }
        #[cfg(windows)]
        {
            #[allow(unused_imports)]
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|error| SplatError::EngineStart {
            engine: spec
                .executable
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| "process".into()),
            detail: error.to_string(),
        })?;
        let pid = child.id();
        if let Some(observer) = spec.observer.as_ref() {
            (observer)(ProcessUpdate::Started { pid });
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log_file: Option<SharedFile> = if let Some(path) = spec.log_path.as_ref() {
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            match tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .await
            {
                Ok(file) => Some(Arc::new(Mutex::new(file))),
                Err(_) => None,
            }
        } else {
            None
        };
        let started = Instant::now();
        *self.child.lock().await = Some(child);

        let cancel_flag = self.cancel_flag.clone();
        let observer = spec.observer.clone();

        let stdout_handle = tokio::spawn(drain_stream(
            stdout,
            ProcessStream::Stdout,
            log_file.clone(),
            observer.clone(),
        ));
        let stderr_handle = tokio::spawn(drain_stream(
            stderr,
            ProcessStream::Stderr,
            log_file,
            observer.clone(),
        ));

        // Start alongside stream draining and child waiting. Starting it after
        // `drain_stream` would mean it only begins once the process has exited.
        let heartbeat_handle = observer.as_ref().map(|obs| {
            let obs = obs.clone();
            let cancel = cancel_flag.clone();
            tokio::spawn(async move {
                loop {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    (obs)(ProcessUpdate::Heartbeat {
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    });
                }
            })
        });

        let mut guard = self.child.lock().await;
        let mut child = match guard.take() {
            Some(child) => child,
            None => {
                return Err(SplatError::Process("子进程句柄在等待之前被移除".into()));
            }
        };
        drop(guard);

        let cancel_flag = self.cancel_flag.clone();
        let status = tokio::select! {
            status = child.wait() => status.map_err(SplatError::Io)?,
            _ = wait_until_cancelled(&cancel_flag) => {
                // 用户点击“取消/停止”后立即终止子进程，而不是等它自然结束。
                let _ = child.kill().await;
                child.wait().await.map_err(SplatError::Io)?
            }
        };
        if let Some(handle) = heartbeat_handle {
            handle.abort();
        }

        let stdout_text = stdout_handle
            .await
            .map_err(|error| SplatError::Process(format!("stdout 读取任务失败：{error}")))??;
        let stderr_text = stderr_handle
            .await
            .map_err(|error| SplatError::Process(format!("stderr 读取任务失败：{error}")))??;

        let cancelled = self.cancel_flag.load(Ordering::SeqCst);
        let exit_code = status.code();
        let success = status.success() && !cancelled;
        Ok(ProcessOutput {
            stdout: stdout_text,
            stderr: stderr_text,
            success,
            exit_code,
            cancelled,
        })
    }

    /// Force-kill any in-flight child. The child is `kill_on_drop` so usually not needed,
    /// but this lets the cancel button take effect immediately.
    pub async fn kill(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.kill().await;
        }
    }
}

async fn drain_stream(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    kind: ProcessStream,
    log_file: Option<SharedFile>,
    observer: Option<ProcessObserver>,
) -> Result<String> {
    let mut stream = match stream {
        Some(stream) => stream,
        None => return Ok(String::new()),
    };
    let mut reader = BufReader::new(&mut stream);
    let mut buffer = String::new();
    let mut collected = String::new();
    loop {
        buffer.clear();
        let read = reader.read_line(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let trimmed = buffer.trim_end_matches(['\n', '\r']);
        if let Some(log) = log_file.as_ref() {
            let mut guard = log.lock().await;
            let _ = guard.write_all(trimmed.as_bytes()).await;
            let _ = guard.write_all(b"\n").await;
        }
        if let Some(obs) = observer.as_ref() {
            (obs)(ProcessUpdate::Line {
                stream: kind,
                line: trimmed.to_owned(),
            });
        }
        collected.push_str(trimmed);
        collected.push('\n');
    }
    Ok(collected)
}

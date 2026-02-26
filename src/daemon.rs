use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::Command;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::{net::TcpListener};

use crate::get_paths;
use crate::protocal::{Request, Response};

#[derive(Serialize, Deserialize)]
pub struct DeamonInfo {
    pub port: u16,
    pub pid: u32
}

pub enum SessionState {
    Running,
    Exited(String)
}

#[derive(Clone)]
struct Session {
    pub stdin_tx: mpsc::Sender<Vec<u8>>,
    pub stdout_tx: broadcast::Sender<Vec<u8>>,
    pub history: Arc<Mutex<VecDeque<Bytes>>>,
    pub state: Arc<Mutex<SessionState>>,
    pub child_handle: Arc<Mutex<tokio::process::Child>>
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

pub async fn run_daemon() -> anyhow::Result<()> {

    let daemon_info_path = get_paths().daemon_info_path;
    if daemon_info_path.exists() {
        let info_content = tokio::fs::read_to_string(&daemon_info_path).await?;
        let info: DeamonInfo = serde_json::from_str(&info_content)?;
        if daemon_running(info.pid).await {
            anyhow::bail!("Daemon is running already");
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let info = DeamonInfo {
        port: listener.local_addr()?.port(),
        pid: std::process::id()
    };
    
    if let Some(parent_dir) = daemon_info_path.parent() {
        tokio::fs::create_dir_all(parent_dir).await?;
    }

    {
        let mut info_file = File::create(&daemon_info_path).await?;
        info_file.write_all(serde_json::to_string_pretty(&info)?.as_bytes()).await?;
    }

    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));

    tokio::spawn(async move {
        loop {
            let _ = tokio::signal::ctrl_c().await;
        }
    });

    loop {
        if let Ok((socket, _)) = listener.accept().await {
            let sessions_cloned: Sessions = sessions.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_request(socket, sessions_cloned).await {
                    eprintln!("error in handling request: {e}");
                }
            });
        }
    }
}

async fn handle_request(
    socket: TcpStream,
    sessions: Sessions
) -> anyhow::Result<()> {

    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let req: Request = serde_json::from_str(&line)?;

    match req {
        Request::Run { name, command } => {
            if let Some(session) = sessions.lock().await.get(&name) {
                respond(&mut writer, Response::SessionNameOccupied(
                    matches!(*session.state.lock().await, SessionState::Exited(_))
                )).await;
            } else {
                match handle_run(sessions, name, command).await {
                    Ok(_) => respond(&mut writer, Response::RunSuccess).await,
                    Err(e) => respond(&mut writer, Response::RunFailed(e.to_string())).await
                };
            }
        }
        Request::Attach { name } => {
            let _ = handle_attach(sessions, name, reader, writer).await;
        }
        Request::List => {
            let session_list: Vec<String> = sessions.lock().await.keys()
                .map(|key| key.to_string()).collect();
            respond(&mut writer, Response::SessionList(session_list)).await;
        }
        Request::Delete { name, force } => {
            let _ = handle_delete(sessions, name, force, writer).await;
        }
    }

    Ok(())
}

async fn respond(
    writer: &mut OwnedWriteHalf,
    response: Response
) {
    if let Ok(s) = serde_json::to_string(&response) {
        let s = s + "\n";
        let _ = writer.write_all(s.as_bytes()).await;
    }
}

async fn handle_run(
    sessions: Sessions,
    name: String,
    command: Vec<String>
) -> anyhow::Result<()> {

    let cmd = &command[0];
    let args = &command[1..];
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let child_handle = Arc::new(Mutex::new(child));

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(64);
    let (stdout_tx, _stdout_rx) = broadcast::channel::<Vec<u8>>(256);

    let session = Session {
        stdin_tx: stdin_tx,
        stdout_tx: stdout_tx.clone(),
        history: Arc::new(Mutex::new(VecDeque::new())),
        state: Arc::new(Mutex::new(SessionState::Running)),
        child_handle: child_handle.clone()
    };
    let history = session.history.clone();
    let state = session.state.clone();

    sessions.lock().await.insert(name, session);

    // launch stdin writer task
    let history_stdin = history.clone();
    tokio::spawn(async move {
        while let Some(data) = stdin_rx.recv().await {
            if let Err(e) = stdin.write_all(&data).await {
                eprintln!("Error writing to stdin: {}", e);
                break;
            }
            if let Err(e) = stdin.flush().await {
                eprintln!("Error flushing stdin: {}", e);
                break;
            }
            {
                let mut history_guard = history_stdin.lock().await;
                history_guard.push_back(data.into());
                if history_guard.len() > 5000 {
                    history_guard.pop_front();
                }
            }
        }
        // loop exits when channel is closed
    });

    // launch wating task
    let state_cloned = state.clone();
    tokio::spawn(async move {
        loop {
            match child_handle.lock().await.try_wait() {
                Ok(Some(status)) => {
                    *state_cloned.lock().await = SessionState::Exited(status.to_string());
                    break;
                }
                Ok(None) => (),
                Err(e) => {
                    *state_cloned.lock().await = SessionState::Exited(e.to_string());
                    break;
                }
            };
            tokio::time::sleep(Duration::from_millis(100)).await;  
        }
    });

    // launch stdout reader task
    let history_stdout = history.clone();
    let state_stdout = state.clone();
    let stdout_tx_stdout = stdout_tx.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        // stdout has a type of ChildStdout,
        // which is for child process to write and for this process to read.
        // so it cannot be flushed from here.
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) => {
                    if let SessionState::Exited(_) = *state_stdout.lock().await {
                        break;
                        // loop exits when state is Exited and stdout.read returns 0
                        // (this is a double check)
                    }
                }
                Ok(n) => {
                    let data = Bytes::copy_from_slice(&buf[..n]);
                    {
                        let mut history_guard = history_stdout.lock().await;
                        history_guard.push_back(data.clone());
                        if history_guard.len() > 5000 {
                            history_guard.pop_front();
                        }
                    }
                    let _ = stdout_tx_stdout.send(data.to_vec());
                }
                Err(_) => break // Error handling?
            }
        }
    });

    // launch stderr reader task
    let history_stderr = history.clone();
    let state_stderr = state.clone();
    let stdout_tx_stderr = stdout_tx.clone();
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        // stderr has a type of ChildStderr,
        // which is for child process to write and for this process to read.
        // so it cannot be flushed from here.
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) => {
                    if let SessionState::Exited(_) = *state_stderr.lock().await {
                        break;
                        // loop exits when state is Exited and stdout.read returns 0
                        // (this is a double check)
                    }
                }
                Ok(n) => {
                    let data = Bytes::copy_from_slice(&buf[..n]);
                    {
                        let mut history_guard = history_stderr.lock().await;
                        history_guard.push_back(data.clone());
                        if history_guard.len() > 5000 {
                            history_guard.pop_front();
                        }
                    }
                    let _ = stdout_tx_stderr.send(data.to_vec());
                }
                Err(_) => break // Error handling?
            }
        }
    });

    Ok(())
}

async fn handle_attach(
    sessions: Sessions,
    name: String,
    mut reader: BufReader<OwnedReadHalf>,
    mut writer: OwnedWriteHalf
) -> anyhow::Result<()> {

    let (stdin_tx, mut stdout_rx, history, state) = {
        let sessions_guard = sessions.lock().await;
        if let Some(session) = sessions_guard.get(&name) {
            (
                session.stdin_tx.clone(),
                session.stdout_tx.subscribe(),
                session.history.clone(),
                session.state.clone()
            )
        } else {
            respond(&mut writer, Response::SessionNotExists).await;
            return Ok(());
        }
    };


    respond(&mut writer, Response::ForwardingReady).await;
    // send history to client
    {
        let history_guard = history.lock().await;
        let history_data: Vec<Bytes> = history_guard.iter().cloned().collect();
        drop(history_guard);

        for chunk in history_data {
            writer.write_all(&chunk).await?;
        }
    }

    // subscribe real-time output
    tokio::spawn(async move {
        loop {
            match stdout_rx.recv().await {
                Ok(data) => {
                    let _ = writer.write_all(&data).await;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    if let SessionState::Exited(ref _e) = *state.lock().await {
                        // todo: display exit info
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => ()
            }
        }
    });

    // forward client input
    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break, // client closed
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    if stdin_tx.send(data).await.is_err() {
                        break;
                        // session dropped
                    }
                },
                Err(_) => break // Error handling?
            };
        }
    });

    Ok(())
}

async fn handle_delete(
    sessions: Sessions,
    name: String,
    force: bool,
    mut writer: OwnedWriteHalf
) -> anyhow::Result<()> {
    
    let (state, handle) = {
        let sessions_guard = sessions.lock().await;
        if let Some(session) = sessions_guard.get(&name) {
            (
                session.state.clone(),
                session.child_handle.clone()
            )
        } else {
            respond(&mut writer, Response::SessionNotExists).await;
            return Ok(());
        }
    };

    if let SessionState::Exited(_) = *state.lock().await {
        sessions.lock().await.remove(&name);
        respond(&mut writer, Response::SessionDeletedSafely).await;
        return Ok(());
    } else {
        if force {
            if handle.lock().await.start_kill().is_err() {
                sessions.lock().await.remove(&name);
                respond(&mut writer, Response::SessionDropped).await;
                return Ok(());
            };
            for _i in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if let SessionState::Exited(_) = *state.lock().await {
                    sessions.lock().await.remove(&name);
                    respond(&mut writer, Response::SessionDeletedForcely).await;
                    return Ok(());
                }
            }
            sessions.lock().await.remove(&name);
            respond(&mut writer, Response::SessionDropped).await;
            return Ok(());
        } else {
            respond(&mut writer, Response::SessionProcessNotExited).await;
            return Ok(());
        }
    }
}

pub async fn daemon_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::path::Path::new(
            &format!("/proc/{}", pid)
        ).exists()
    }
    #[cfg(windows)]
    {
        match Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV"])
        .output().await {
            Ok(output) => {
                let output_str = String::from_utf8_lossy(&output.stdout);
                output_str.contains(&format!("{pid}"))
            }
            Err(_) => return false
        }
    }
}
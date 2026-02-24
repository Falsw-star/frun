use std::{sync::Arc, time::Duration};
use std::io::Write;

use clap::{ArgAction, Parser, Subcommand};
use frun::{daemon::{DeamonInfo, daemon_running, run_daemon}, get_paths, protocal::{Request, Response}};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, net::{TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}}, process::Command, select, sync::Mutex};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Run {
        name: String,
        #[arg(raw = true, action = ArgAction::Append)]
        command: Vec<String>
    },
    #[command(alias = "a")]
    Attach { name: String },
    #[command(alias = "ls")]
    List,
    Daemon,
    #[command(alias = "del")]
    Delete {
        name: String,
        #[arg(long)]
        force: bool
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Run { name, command } => {

            let (_, _, res) = send_request(
                Request::Run { name, command }
            ).await?;
            match res {
                Response::RunSuccess => {
                    println!("Success!");
                },
                Response::RunFailed(msg) => {
                    println!("Failed to run: {}", msg);
                }
                _ => anyhow::bail!("Daemon responded with wrong response type")
            } Ok(())
        }
        Cmd::Attach { name } => {

            let (reader, writer, res) = send_request(
                Request::Attach { name }
            ).await?;
            match res {
                Response::ForwardingReady => {
                    start_forwarding(reader, writer).await?;
                }
                Response::SessionNotExists => {
                    println!("Session not exists");
                }
                _ => anyhow::bail!("Daemon responded with wrong response type")
            } Ok(())
        }
        Cmd::List => {

            let (_, _, res) = send_request(
                Request::List
            ).await?;
            match res {
                Response::SessionList(list) => {
                    if list.len() == 0 {
                        println!("There aren't any session created.")
                    } else {
                        for name in &list {
                            println!("    {}", name);
                        }
                    }
                }
                _ => anyhow::bail!("Daemon responded with wrong response type")
            } Ok(())
        }
        Cmd::Daemon => {
            run_daemon().await?;
            Ok(())
        }
        Cmd::Delete { name, force } => {
            let (_, _, res) = send_request(
                Request::Delete { name: name.clone(), force }
            ).await?;

            match res {
                Response::SessionDeletedSafely => {
                    println!("Process already exited and session deleted safely.")
                }
                Response::SessionProcessNotExited => {
                    print!("Process still running! Do you want to exit it forcely? (y): ");
                    std::io::stdout().flush()?;

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;

                    let input = input.trim().to_lowercase();
                    if input == "y" || input == "yes" {

                        let status = std::process::Command::new(std::env::current_exe()?)
                            .args(["delete", &name, "--force"])
                            .status()?;

                        if !status.success() {
                            std::process::exit(status.code().unwrap_or(1));
                        }
                    } else {
                        println!("Operation canceled.")
                    }
                }
                Response::SessionDeletedForcely => {
                    println!("Process terminated and session exited.");
                }
                Response::SessionDropped => {
                    println!("Session dropped and process exited unsafely.");
                }
                Response::SessionNotExists => {
                    println!("Session not exists");
                }
                _ => anyhow::bail!("Daemon responded with wrong response type")
            } Ok(())
        }
    }

}

async fn send_request(req: Request) -> anyhow::Result<(BufReader<OwnedReadHalf>, OwnedWriteHalf, Response)> {
    let socket = get_daemon().await?;
    let (reader, mut writer) = socket.into_split();
    let mut reader = BufReader::new(reader);

    let s = serde_json::to_string(&req)? + "\n";
    writer.write_all(s.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let res: Response = serde_json::from_str(&line)?;
    Ok((reader, writer, res))
}

async fn start_forwarding(
    mut reader: BufReader<OwnedReadHalf>,
    mut writer: OwnedWriteHalf
) -> anyhow::Result<()> {

    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let exit = Arc::new(Mutex::new(false));

    let stdin_task = tokio::spawn({
        let exit = exit.clone();
        async move {
            let mut buf = [0u8; 1024];
            loop {
                select! {
                    read_result = stdin.read(&mut buf) => {
                        match read_result {
                            // user entered Ctrl+D (EOF)
                            Ok(0) => break,
                            Ok(n) => {
                                if let Err(e) = writer.write_all(&buf[..n]).await {
                                    eprintln!("Failed to write to stream: {}", e);
                                    break;
                                }
                                if let Err(e) = writer.flush().await {
                                    eprintln!("Failed to flush stream: {}", e);
                                    break;
                                }
                            }
                            Err(_) => break
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        break;
                    }
                }
            }
            *exit.lock().await = true;
        }
    });

    let stdout_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {

            select! {
                read_result = reader.read(&mut buf) => {
                    match read_result {
                        // attach -> session closed -> receiver closed
                        // -> writer dropped -> reader returns 0
                        Ok(0) => break,
                        Ok(n) => {
                            if let Err(e) = stdout.write_all(&buf[..n]).await {
                                eprintln!("Error writing to stdout: {}", e);
                                break;
                            }
                            if let Err(e) = stdout.flush().await {
                                eprintln!("Error flushing stdout: {}", e);
                                break;
                            }
                        }
                        Err(_) => break
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    if *exit.lock().await {
                        break; // exit safely
                    }
                }
            }
        }
    });

    stdin_task.await?;
    stdout_task.await?;

    Ok(())
}


const MAX_RETRIES: usize = 5;

async fn get_daemon() -> anyhow::Result<TcpStream> {

    // the sturcture may not the best,
    // but it ensures there aren't dead loops
    for _i in 0..MAX_RETRIES {
        let daemon_info_path = &get_paths().daemon_info_path;

        if daemon_info_path.exists() {
            let info_content = tokio::fs::read_to_string(&daemon_info_path).await?;
            let daemon_info: DeamonInfo = serde_json::from_str(&info_content)?;

            let addr = format!("127.0.0.1:{}", daemon_info.port);
            match TcpStream::connect(addr).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    if daemon_running(daemon_info.pid).await {
                        eprintln!("Failed to connect to daemon: {}", e);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    } else {
                        tokio::fs::remove_file(daemon_info_path).await?;
                    }
                }
            }
        } else {
            if let Err(e) = create_daemon_process().await {
                eprintln!("{}", e);
            }
        }
    }

    anyhow::bail!("Failed to run after {} retries", MAX_RETRIES);
}

async fn create_daemon_process() -> anyhow::Result<()> {

    let mut child = Command::new(std::env::current_exe()?)
        .kill_on_drop(false)
        .arg("daemon")
        .spawn()?;

    let daemon_info_path = &get_paths().daemon_info_path;
    for _i in 0..10 {
        if !daemon_info_path.exists() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            break;
        }
    }

    if !daemon_info_path.exists() {
        child.kill().await?;
        anyhow::bail!("Daemon failed to start")
    }
        
    Ok(())
}


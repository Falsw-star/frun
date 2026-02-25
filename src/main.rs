use std::{sync::Arc, time::Duration};

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
                Request::Run { name: name.clone(), command: command.clone() }
            ).await?;
            match res {
                Response::SessionNameOccupied(exited) => {
                    println!(
                        "[{}] {} '{}' {}",
                        console::style("W").bold().yellow(),
                        "There's already a session called",
                        console::style(&name).bold().cyan(),
                        "exists."
                    );
                    if exited {
                        let confirm = dialoguer::Confirm::new()
                            .with_prompt(format!(
                                "[{}] {} '{}' {}",
                                console::style("I").bold().blue(),
                                "The process in session",
                                console::style(&name).bold().cyan(),
                                "is already exited. Delete it and run new one?"))
                            .default(false)
                            .interact()?;
                        
                        if confirm {
                            let status = std::process::Command::new(std::env::current_exe()?)
                                .args(["delete", &name])
                                .status()?;
                            if !status.success() {
                                std::process::exit(status.code().unwrap_or(1));
                            }
                            let status = std::process::Command::new(std::env::current_exe()?)
                                .args(["run", &name, "--"])
                                .args(command)
                                .status()?;
                            if !status.success() {
                                std::process::exit(status.code().unwrap_or(1));
                            }
                        } else {
                            println!("[{}] Operation cancelled.",
                                console::style("I").bold().blue());
                        }
                    } else {
                        println!(
                            "[{}] {} '{}' {}",
                            console::style("W").bold().yellow(),
                            "The process in session",
                            console::style(&name).bold().cyan(),
                            "is still running. Please delete it manually first."
                        );
                        println!("[{}] Operation cancelled.",
                            console::style("I").bold().blue());
                    }
                }
                Response::RunSuccess => {
                    println!(
                        "[{}] {} : '{}'",
                        console::style("S").bold().green(),
                        console::style("Successfully started session").green(),
                        console::style(name).bold().cyan()
                    );
                },
                Response::RunFailed(msg) => {
                    eprintln!(
                        "[{}] {} : '{}' : {}",
                        console::style("E").bold().red(),
                        console::style("Failed to run session").red(),
                        console::style(name).bold().cyan(),
                        msg
                    );
                }
                _ => anyhow::bail!(
                    "[{}] {} : {:?}",
                    console::style("E").bold().red(),
                    console::style("Daemon responded with wrong response type").red(),
                    res
                )
            } Ok(())
        }
        Cmd::Attach { name } => {

            let (reader, writer, res) = send_request(
                Request::Attach { name: name.clone() }
            ).await?;
            match res {
                Response::ForwardingReady => {
                    println!("[{}] Attaching to session: '{}'", 
                        console::style("T").bold().magenta(),
                        console::style(&name).bold().cyan());
                    start_forwarding(reader, writer).await?;
                    println!("[{}] Detached from session: '{}'", 
                        console::style("T").bold().magenta(),
                        console::style(&name).bold().cyan());
                    std::process::exit(0);
                }
                Response::SessionNotExists => {
                    println!("[{}] No session named '{}'",
                        console::style("I").bold().blue(),
                        console::style(name).bold().cyan());
                }
                _ => anyhow::bail!(
                    "[{}] {} : {:?}",
                    console::style("E").bold().red(),
                    console::style("Daemon responded with wrong response type").red(),
                    res
                )
            } Ok(())
        }
        Cmd::List => {

            let (_, _, res) = send_request(
                Request::List
            ).await?;
            match res {
                Response::SessionList(list) => {
                    if list.len() == 0 {
                        println!("[{}] There aren't any session created.",
                            console::style("I").bold().blue())
                    } else {
                        println!("[{}] All {} sessions listed as below:",
                            console::style("I").bold().blue(),
                            console::style(list.len()).blue());
                        for name in &list {
                            println!("    {}", console::style(name).bold().cyan());
                        }
                    }
                }
                _ => anyhow::bail!(
                    "[{}] {} : {:?}",
                    console::style("E").bold().red(),
                    console::style("Daemon responded with wrong response type").red(),
                    res
                )
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
                    println!("[{}] Process already exited and session '{}' deleted safely.",
                        console::style("S").bold().green(),
                        console::style(&name).bold().cyan())
                }
                Response::SessionProcessNotExited => {
                    let confirm = dialoguer::Confirm::new()
                        .with_prompt(format!(
                            "[{}] Process in session '{}' is still running! Terminate it forcibly?",
                            console::style("W").bold().yellow(),
                            console::style(&name).bold().cyan()))
                        .default(false)
                        .interact()?;
                    if confirm {
                        let status = std::process::Command::new(std::env::current_exe()?)
                            .args(["delete", &name, "--force"])
                            .status()?;

                        if !status.success() {
                            std::process::exit(status.code().unwrap_or(1));
                        }
                    } else {
                        println!("[{}] Operation cancelled.",
                            console::style("I").bold().blue())
                    }
                }
                Response::SessionDeletedForcely => {
                    println!("[{}] Process terminated and session '{}' deleted.",
                        console::style("S").bold().green(),
                        console::style(name).bold().cyan());
                }
                Response::SessionDropped => {
                    println!("[{}] Session '{}' dropped and process exited unsafely.",
                        console::style("W").bold().yellow(),
                        console::style(name).bold().cyan());
                }
                Response::SessionNotExists => {
                    println!("[{}] No session named '{}'",
                        console::style("I").bold().blue(),
                        console::style(name).bold().cyan());
                }
                _ => anyhow::bail!(
                    "[{}] {} : {:?}",
                    console::style("E").bold().red(),
                    console::style("Daemon responded with wrong response type").red(),
                    res
                )
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
            match TcpStream::connect(&addr).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    if daemon_running(daemon_info.pid).await {
                        eprintln!(
                            "[{}] {} '{}' : {}",
                            console::style("E").red(),
                            console::style("Failed to connect to daemon on").red(),
                            console::style(&addr),
                            err
                        );
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

    anyhow::bail!(
        "[{}] {} {} {}",
        console::style("E").red(),
        console::style("Failed to run after").red(),
        console::style(MAX_RETRIES).yellow(),
        console::style("retries.").red(),
    );
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
        anyhow::bail!(
            "[{}] {}",
            console::style("E").red(),
            console::style("Daemon failed to start").red()
        )
    }
        
    Ok(())
}


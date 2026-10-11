use clap::Parser;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod protocol;
mod service;

use protocol::{Request, Response};
use service::HelloService;

#[derive(Parser, Debug)]
#[command(name = "linux-hellod", about = "Linux-Hello Biometric Authentication Daemon")]
struct Cli {
    #[arg(short, long, default_value = "/run/linux-hello.sock")]
    socket: PathBuf,

    #[arg(short, long, default_value = "models")]
    models_dir: PathBuf,

    #[arg(long, default_value = "/var/lib/linux-hello/models")]
    storage_dir: PathBuf,

    #[arg(long)]
    camera: Option<PathBuf>,

    #[arg(long)]
    threshold: Option<f32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    let args = Cli::parse();

    log::info!("Starting linux-hellod daemon...");
    log::info!("Socket path: {:?}", args.socket);
    log::info!("Models directory: {:?}", args.models_dir);
    log::info!("Storage directory: {:?}", args.storage_dir);

    // If models_dir doesn't exist relative to pwd, check default system locations
    let models_dir = if args.models_dir.exists() {
        args.models_dir
    } else if Path::new("/usr/share/linux-hello/models").exists() {
        PathBuf::from("/usr/share/linux-hello/models")
    } else {
        args.models_dir
    };

    let service = Arc::new(HelloService::new(
        &models_dir,
        &args.storage_dir,
        args.camera.as_deref(),
        args.threshold,
    )?);

    // Clean up old socket file if it exists
    if args.socket.exists() {
        let _ = fs::remove_file(&args.socket);
    }

    if let Some(parent) = args.socket.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let listener = UnixListener::bind(&args.socket)?;
    let _ = fs::set_permissions(&args.socket, fs::Permissions::from_mode(0o666));

    log::info!("Listening on Unix socket: {:?}", args.socket);

    let running = Arc::new(AtomicBool::new(true));
    let _r = running.clone();

    // Signal handling
    unsafe {
        libc::signal(libc::SIGINT, sig_handler as usize);
        libc::signal(libc::SIGTERM, sig_handler as usize);
    }

    listener.set_nonblocking(true)?;

    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let service = service.clone();
                handle_client(stream, &service);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => {
                log::error!("Error accepting connection: {}", e);
            }
        }
    }

    log::info!("Shutting down linux-hellod...");
    let _ = fs::remove_file(&args.socket);
    Ok(())
}

extern "C" fn sig_handler(_: libc::c_int) {
    // Process termination
    std::process::exit(0);
}

fn handle_client(mut stream: UnixStream, service: &HelloService) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });

    let mut line = String::new();
    if let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            return;
        }

        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(req) => service.handle_request(req),
            Err(e) => Response::Error {
                message: format!("Malformed request JSON: {}", e),
            },
        };

        if let Ok(resp_json) = serde_json::to_string(&resp) {
            let _ = writeln!(stream, "{}", resp_json);
            let _ = stream.flush();
        }
    }
}

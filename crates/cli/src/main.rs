//! `retv-bridge-cli [--port 5577] [--no-open] [--site URL]`: the bridge in a
//! terminal. Fetches Google's platform-tools on first run (pinned SHA-256),
//! starts the bridge's private adb server on 5039 (never the person's 5037),
//! serves the site on ws://127.0.0.1:<port> and prints every command.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use retv_bridge_adb::server_proc::{restart_adb_server, start_adb_server, BRIDGE_ADB_PORT};
use retv_bridge_adb::tools::{ensure_adb, host_pin, ReqwestGet, ToolsSource};
use retv_bridge_adb::{AdbHost, BoxFuture, Discoverer, HttpsApkFetcher, RealAdb, ServerRevive};
use retv_bridge_cli::{
    code_block, connect_link, data_dir, new_token, open_browser, parse_args, should_print,
    site_to_open, ArgsError, DEFAULT_SITE, USAGE,
};
use retv_bridge_core::api::{BridgeAdb, Clock};
use retv_bridge_core::events::BridgeEvent;
use retv_bridge_core::pairing::OsCodeSource;
use retv_bridge_core::protocol::{ALLOWED_ORIGINS, BRIDGE_HOST};
use retv_bridge_core::server::{start_server, ServerConfig, StartError};
use retv_bridge_core::validate::MdnsService;

struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

fn discoverer() -> Discoverer {
    Arc::new(|| -> BoxFuture<'static, Vec<MdnsService>> {
        Box::pin(async {
            match retv_bridge_mdns::discover(
                retv_bridge_mdns::DEFAULT_TIMEOUT,
                &retv_bridge_mdns::SystemInterfaces,
            )
            .await
            {
                Ok(found) => found.iter().map(MdnsService::from).collect(),
                Err(e) => {
                    eprintln!("[bridge] mDNS discovery failed: {e}");
                    Vec::new()
                }
            }
        })
    })
}

/// Ctrl+C, or (macOS, Linux) SIGTERM: either stops the bridge and its adb server.
async fn stop_requested() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(ArgsError::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(ArgsError::Bad(m)) => {
            eprintln!("{m}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let site = args
        .site
        .clone()
        .or_else(|| std::env::var("RETV_SITE_URL").ok())
        .unwrap_or_else(|| DEFAULT_SITE.to_string());

    // Google's adb, fetched once into the data folder and checked against its pin.
    let tools_dir = data_dir().join("platform-tools");
    let http = match ReqwestGet::new() {
        Ok(h) => Arc::new(h),
        Err(e) => {
            eprintln!("Could not set up downloads: {e}");
            return ExitCode::FAILURE;
        }
    };
    let pin = host_pin();
    let adb_path = match ensure_adb(
        &tools_dir,
        pin,
        ToolsSource::Download(http),
        |got, total| {
            if got == 0 {
                println!(
                    "Downloading Android platform-tools r{} ({} MB)…",
                    pin.revision,
                    total / 1_000_000
                );
            }
        },
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Could not get Android platform-tools: {e}");
            return ExitCode::FAILURE;
        }
    };
    let adb_server = match start_adb_server(&adb_path, BRIDGE_ADB_PORT).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Could not start adb: {e}");
            if let Some(detail) = e.detail() {
                eprintln!("  ({detail})");
            }
            return ExitCode::FAILURE;
        }
    };

    // A server that dies (killed by another program) is started again.
    let revive = {
        let adb_path = adb_path.clone();
        Arc::new(ServerRevive::new(Arc::new(
            move || -> BoxFuture<'static, bool> {
                let adb_path = adb_path.clone();
                Box::pin(async move {
                    match restart_adb_server(&adb_path, BRIDGE_ADB_PORT).await {
                        // Still there, only slow: left alone.
                        Ok(None) => false,
                        Ok(Some(_)) => {
                            eprintln!("[bridge] the adb server had stopped: started it again");
                            true
                        }
                        Err(e) => {
                            eprintln!("[bridge] could not start adb: {e}");
                            false
                        }
                    }
                })
            },
        )))
    };
    let adb: Arc<dyn BridgeAdb> = Arc::new(
        RealAdb::new(AdbHost::new(adb_server.port()), discoverer()).with_revive(revive.clone()),
    );
    let fetcher = match HttpsApkFetcher::new() {
        Ok(f) => Arc::new(f),
        Err(e) => {
            eprintln!("Could not set up downloads: {e}");
            return ExitCode::FAILURE;
        }
    };
    let token = new_token(); // leakcheck:allow
    let server = match start_server(ServerConfig {
        token: token.clone(), // leakcheck:allow
        port: args.port,
        adb: adb.clone(),
        fetcher,
        clock: Arc::new(SystemClock),
        codes: Arc::new(OsCodeSource),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
    .await
    {
        Ok(s) => s,
        Err(StartError::PortInUse) => {
            eprintln!(
                "Port {} is already in use: the bridge seems to be running in another window.",
                args.port
            );
            let _ = adb_server.stop().await;
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("Could not start the bridge: {e}");
            let _ = adb_server.stop().await;
            return ExitCode::FAILURE;
        }
    };

    // Everything the bridge reports, as it happens.
    let mut events = server.events();
    let printer = tokio::spawn(async move {
        let clock = SystemClock;
        let mut printed: Option<(String, u64)> = None;
        loop {
            match events.recv().await {
                Ok(BridgeEvent::Log(line)) => println!("{line}"),
                Ok(BridgeEvent::PairCode { code, .. }) => {
                    let now = clock.now_ms();
                    if should_print(printed.as_ref().map(|(c, t)| (c.as_str(), *t)), &code, now) {
                        println!("\n{}\nType these 6 digits on the ReTV site. The code works for 2 minutes.\n", code_block(&code));
                        printed = Some((code, now));
                    }
                }
                Ok(BridgeEvent::PairingDisabled) => eprintln!(
                    "The bridge has handed out too many site codes. To connect the site again, restart the bridge (Ctrl+C, then start it again)."
                ),
                Ok(BridgeEvent::TvConnected { serial, model }) => println!("[bridge] TV connected: {serial} {model}"),
                Ok(BridgeEvent::TvDisconnected { serial }) => println!("[bridge] TV disconnected: {serial}"),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => eprintln!("[bridge] ({n} log lines skipped)"),
                Err(_) => break,
            }
        }
    });

    if !adb.available().await {
        eprintln!("Warning: the adb server does not answer.");
    }
    println!(
        "ReTV Bridge {}: ws://{BRIDGE_HOST}:{}",
        env!("CARGO_PKG_VERSION"),
        server.port()
    );
    println!("Allowed sites: {}", ALLOWED_ORIGINS.join(", "));
    println!();
    match site_to_open(&site) {
        None => {
            eprintln!("{site} is not an allowed site: the bridge does not open it or print a link with the token.");
            println!("Open the ReTV site yourself: it asks for 6 digits, and the bridge shows them here.");
        }
        Some(origin) => {
            let link = connect_link(&origin, &token);
            println!("Open: {link}");
            println!(
                "Or open the site yourself: it asks for 6 digits, and the bridge shows them here."
            );
            if args.open {
                if let Err(e) = open_browser(&link) {
                    eprintln!("Could not open the browser: {e}");
                }
            }
        }
    }
    println!();
    println!("Every command sent to a TV is printed below. Stop the bridge: Ctrl+C.");

    stop_requested().await;
    println!("Stopping the bridge…");
    revive.stop();
    server.shutdown().await;
    // Let the printer show the last lines. Its stream ends when the server's last
    // socket task lets go of the event sender, which can outlive `shutdown`
    // (a socket still closing), hence the bound.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), printer).await;
    let _ = adb_server.stop().await;
    ExitCode::SUCCESS
}

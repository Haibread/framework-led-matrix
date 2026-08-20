//! Drives the two Framework 16 LED Matrix modules.
//!
//! Each module gets its own thread running a fixed-rate loop: step the scene,
//! draw it into a 9x34 greyscale canvas, push the canvas over USB serial. The
//! main task does nothing but wait for a signal and then stop the panels
//! cleanly, so the LEDs never stay lit after the process exits.

mod audio;
mod canvas;
mod cli;
mod control;
mod device;
mod font;
mod poller;
mod runner;
mod scene;
mod server;
mod state;
mod system;
mod tui;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::task::JoinSet;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use cli::Cli;
use control::SceneSpec;
use control::{PanelName, Request, Response};
use device::serial::SerialMatrix;
use device::terminal::TerminalMatrix;
use device::{ColorMode, Panel};
use runner::PanelSettings;
use scene::AnyScene;

/// Terminal columns the preview panels are drawn at.
const LEFT_PREVIEW_COLUMN: usize = 3;
const RIGHT_PREVIEW_COLUMN: usize = 27;

/// Everything needed to bring one panel up.
struct PanelSpec {
    name: PanelName,
    device: String,
    scene: SceneSpec,
    preview_column: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(cli::Command::Tui) = &cli.command {
        return tui::run(cli.socket_path());
    }
    if let Some(command) = &cli.command {
        return send(&cli.socket_path(), &request_for(command));
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cli.log_filter))
        .with_writer(std::io::stderr)
        .init();

    // What was asked for wins, then what was saved, then the defaults.
    let state_path = cli.state_path();
    let saved = state::load(&state_path);
    let brightness = cli.brightness_for(&saved);

    let specs = [
        PanelSpec {
            name: PanelName::Left,
            device: cli.left_device.clone(),
            scene: cli.scene_for(PanelName::Left, &saved),
            preview_column: LEFT_PREVIEW_COLUMN,
        },
        PanelSpec {
            name: PanelName::Right,
            device: cli.right_device.clone(),
            scene: cli.scene_for(PanelName::Right, &saved),
            preview_column: RIGHT_PREVIEW_COLUMN,
        },
    ];

    let shutdown = Arc::new(AtomicBool::new(false));
    let mut panels = JoinSet::new();
    let mut channels = HashMap::new();
    let mut mirrors = HashMap::new();
    let mut showing = HashMap::new();

    for (index, spec) in specs.into_iter().enumerate() {
        // Offset the seed per panel, or both games play out identically.
        let seed = cli.seed.map(|seed| seed.wrapping_add(index as u64));
        let label = spec.name.label();
        // A stack takes the colour mode of the scene at its head.
        let head = spec.scene.scenes().first().copied();
        let Some(head) = head else { continue };
        if head == scene::SceneKind::Off && spec.scene.scenes().len() == 1 {
            info!(panel = label, "panel disabled");
            continue;
        }
        let mode = cli.color_mode.resolve(head.preferred_color_mode());
        let scene = AnyScene::stack(&spec.scene, seed, mode)
            .with_context(|| format!("building the {label} panel"))?;

        let stop = Arc::clone(&shutdown);
        let settings = PanelSettings {
            label,
            fps: cli.fps,
            brightness,
            mode,
        };
        let (device, column, simulate) = (spec.device, spec.preview_column, cli.simulate);

        // A factory, not a ready-made panel: the runner reopens the module when
        // it stops responding, which is what a suspend or an unplug looks like,
        // and again when a scene needs the other colour mode.
        let open = move |mode| open_panel(label, &device, column, simulate, mode);

        let (sender, commands) = channel();
        channels.insert(spec.name, sender);
        let mirror = Arc::new(Mutex::new(canvas::Canvas::new()));
        mirrors.insert(spec.name, Arc::clone(&mirror));
        showing.insert(spec.name, spec.scene.clone());

        // The serial writes are blocking, so each panel owns a blocking thread
        // rather than pretending to be async.
        panels.spawn_blocking(move || {
            runner::run_panel(settings, open, scene, &commands, &mirror, &stop)
        });
    }

    if panels.is_empty() {
        warn!("both panels are off, nothing to do");
        return Ok(());
    }

    let socket = cli.socket_path();
    let control = server::Control::new(
        channels,
        showing,
        cli.color_mode,
        cli.seed,
        mirrors,
        brightness,
        state_path,
    );

    tokio::select! {
        () = shutdown_signal() => info!("shutdown signal received"),
        Some(finished) = panels.join_next() => report(finished),
        result = server::serve(socket.clone(), control) => {
            // Reached only when the socket gives up. The panels are the point
            // and the socket is a convenience, so losing it must not take them
            // down with it — which is exactly what an unbindable path used to
            // do, panels and all.
            match result {
                Ok(()) => warn!("the control socket closed"),
                Err(error) => error!(?error, "the control socket stopped"),
            }
            shutdown_signal().await;
            info!("shutdown signal received");
        }
    }

    // Leaving the socket behind would make the next start think a daemon is
    // already listening until it probes and finds nobody.
    let _ = std::fs::remove_file(&socket);

    shutdown.store(true, Ordering::Relaxed);
    while let Some(finished) = panels.join_next().await {
        report(finished);
    }

    Ok(())
}

/// Opens one panel, on hardware or in the terminal.
fn open_panel(
    label: &'static str,
    device: &str,
    preview_column: usize,
    simulate: bool,
    mode: ColorMode,
) -> Result<Panel> {
    if simulate {
        return Ok(Panel::Terminal(TerminalMatrix::new(
            label,
            preview_column,
            mode,
        )));
    }

    let matrix = SerialMatrix::open(device, mode).with_context(|| {
        format!(
            "opening the {label} panel — is the udev rule installed? \
             Try --simulate to run without hardware"
        )
    })?;
    Ok(Panel::Serial(matrix))
}

/// Turns a subcommand into the request that goes over the socket.
fn request_for(command: &cli::Command) -> Request {
    match command {
        cli::Command::Set { panel, scene } => Request::Set {
            panel: *panel,
            scene: scene.clone(),
        },
        cli::Command::Brightness { level } => Request::Brightness(*level),
        // The interface never reaches here; it is handled before this, and
        // asking a daemon for its status is the harmless thing to do anyway.
        cli::Command::Status | cli::Command::Tui => Request::Status,
    }
}

/// Sends one request to a running daemon and prints its answer.
fn send(socket: &Path, request: &Request) -> Result<()> {
    let stream = UnixStream::connect(socket).with_context(|| {
        format!(
            "no daemon listening on {} — is ledmat running?",
            socket.display()
        )
    })?;

    let mut writer = &stream;
    writeln!(writer, "{request}").context("sending the request")?;
    writer.flush().context("sending the request")?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .context("reading the answer")?;

    let response: Response = line.parse()?;
    if response.succeeded() {
        println!("{}", response.message());
        Ok(())
    } else {
        anyhow::bail!("{}", response.message())
    }
}

/// Logs how a panel thread ended.
fn report(finished: Result<Result<()>, tokio::task::JoinError>) {
    match finished {
        Ok(Ok(())) => {}
        Ok(Err(error)) => error!(?error, "panel failed"),
        Err(error) => error!(?error, "panel thread crashed"),
    }
}

/// Completes on `SIGTERM` or `SIGINT`.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(handler) => handler,
        Err(error) => {
            error!(?error, "cannot listen for SIGINT");
            return;
        }
    };
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(handler) => handler,
        Err(error) => {
            error!(?error, "cannot listen for SIGTERM");
            return;
        }
    };

    tokio::select! {
        _ = interrupt.recv() => {},
        _ = terminate.recv() => {},
    }
}

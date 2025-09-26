use std::{
    io::Read,
    process::exit,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use eframe::{App, egui};
use portable_pty::{CommandBuilder, PtyPair, native_pty_system};

/// Differentiates whether the incoming data came from stdout or stderr.
#[derive(Debug, Clone, Copy)]
pub enum CommandStream {
    Stdout,
    Stderr,
}

/// Minimal wrapper around a PTY that keeps collected output for rendering.
pub struct PtyTerminal {
    pair: PtyPair,
    buffer: Arc<Mutex<String>>,
    reader_thread: Option<JoinHandle<()>>,
    wait_thread: Option<JoinHandle<()>>,
}

impl PtyTerminal {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let system = native_pty_system();
        let pair = system.openpty(Default::default())?;

        Ok(Self {
            pair,
            buffer: Arc::new(Mutex::new(String::new())),
            reader_thread: None,
            wait_thread: None,
        })
    }

    /// Ingests new text emitted by a subprocess, tagging stderr for clarity.
    pub fn push_output(&self, stream: CommandStream, chunk: &str) {
        Self::write_chunk(&self.buffer, stream, chunk);
    }

    /// Very small egui renderer that shows the collected PTY buffer.
    pub fn ui(&self, ui: &mut egui::Ui) {
        use egui::ScrollArea;

        ui.heading("PTY Output");
        ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            let buffer = self.buffer.lock().expect("terminal buffer poisoned");
            ui.code(buffer.as_str());
        });
    }

    /// Helper for spawning a command on the PTY and streaming its output into the buffer.
    pub fn spawn_command(
        &mut self,
        command: CommandBuilder,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.wait_thread.take() {
            let _ = handle.join();
        }

        self.push_output(CommandStream::Stdout, "Launching command...\n");

        let mut child = self.pair.slave.spawn_command(command)?;
        let reader = self.pair.master.try_clone_reader()?;
        let buffer_for_output = Arc::clone(&self.buffer);

        self.reader_thread = Some(thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]);
                        PtyTerminal::write_chunk(&buffer_for_output, CommandStream::Stdout, &chunk);
                    }
                    Err(err) => {
                        let message = format!("reader error: {err}\n");
                        PtyTerminal::write_chunk(
                            &buffer_for_output,
                            CommandStream::Stderr,
                            &message,
                        );
                        break;
                    }
                }
            }
        }));

        let buffer_for_wait = Arc::clone(&self.buffer);
        self.wait_thread = Some(thread::spawn(move || match child.wait() {
            Ok(status) => {
                if status.success() {
                    PtyTerminal::write_chunk(
                        &buffer_for_wait,
                        CommandStream::Stdout,
                        "Command completed successfully.\n",
                    );
                } else {
                    let notice = format!("Command exited with status {}.\n", status.exit_code());
                    PtyTerminal::write_chunk(&buffer_for_wait, CommandStream::Stderr, &notice);
                }
            }
            Err(err) => {
                let message = format!("Failed to wait on command: {err}\n");
                PtyTerminal::write_chunk(&buffer_for_wait, CommandStream::Stderr, &message);
            }
        }));

        Ok(())
    }

    fn write_chunk(buffer: &Arc<Mutex<String>>, stream: CommandStream, chunk: &str) {
        let mut buffer = buffer.lock().expect("terminal buffer poisoned");
        match stream {
            CommandStream::Stdout => buffer.push_str(chunk),
            CommandStream::Stderr => {
                if !buffer.ends_with('\n') {
                    buffer.push('\n');
                }
                buffer.push_str("[stderr]\n");
                buffer.push_str(chunk);
            }
        }
    }
}

/// Basic eframe application that keeps a single PTY terminal instance.
struct TerminalApp {
    terminal: PtyTerminal,
}

impl TerminalApp {
    fn new(cmd: CommandBuilder) -> Self {
        let mut terminal = PtyTerminal::new().expect("failed to open PTY");
        terminal.push_output(
            CommandStream::Stdout,
            "PTY initialized. Ready to attach commands.\n",
        );
        if let Err(err) = terminal.spawn_command(cmd) {
            let message = format!("Failed to spawn command: {err}\n");
            terminal.push_output(CommandStream::Stderr, &message);
        }
        Self { terminal }
    }
}

impl App for TerminalApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.terminal.ui(ui);
        });
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions::default();
    let target = rfd::FileDialog::new()
        .set_title("変換する対象を選択してください")
        .pick_file();
    if target.is_none() {
        native_dialog::DialogBuilder::message()
            .set_title("エラー")
            .set_text("対象を選択してください")
            .set_level(native_dialog::MessageLevel::Warning)
            .alert();
        exit(1);
    }
    let output = rfd::FileDialog::new()
        .set_title("変換したデータのセーブ先を選択してください")
        .save_file();
    if output.is_none() {
        native_dialog::DialogBuilder::message()
            .set_title("エラー")
            .set_text("出力先を選択してください")
            .set_level(native_dialog::MessageLevel::Warning)
            .alert();
        exit(1);
    }

    let mut command = CommandBuilder::new("ffmpeg");
    command.args(vec![
        "-i",
        target.unwrap().to_str().unwrap(),
        output.unwrap().to_str().unwrap(),
    ]);

    let command_for_app = command.clone();

    eframe::run_native(
        "simpleffmpeg",
        options,
        Box::new(move |_cc| Ok(Box::new(TerminalApp::new(command_for_app.clone())))),
    )
}

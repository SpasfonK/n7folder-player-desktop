//! Moteur audio : la lecture (périphérique de sortie + `rodio`) tourne sur SON PROPRE thread
//! natif, jamais partagée entre threads Tauri. Le reste de l'application ne lui parle que par
//! messages (canal `mpsc`) : aucun souci de `Send`/`Sync` sur les types de `rodio`, qui restent
//! du début à la fin sur le thread qui les a créés.

use serde::Serialize;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tauri::Emitter;

pub enum AudioCommand {
    Load(String),
    Play,
    Pause,
    Stop,
    SetVolume(f32),
    Seek(Duration),
}

/// Émis plusieurs fois par seconde : position de lecture et état, pour la barre de progression.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerTick {
    pub path: Option<String>,
    pub position_ms: u64,
    pub playing: bool,
    /// Vrai une seule fois, sur le tick qui suit la fin naturelle de la piste (jamais répété).
    pub ended: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerError {
    pub message: String,
}

/// Démarre le thread audio et renvoie l'expéditeur de commandes à conserver dans l'état Tauri.
/// Si le périphérique audio est indisponible, le thread émet un seul événement d'erreur puis
/// s'arrête : le reste de l'application (bibliothèque, réglages) reste utilisable sans lecture.
pub fn spawn(app: tauri::AppHandle) -> Sender<AudioCommand> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || run(rx, app));
    tx
}

/// Position de lecture reconstituée à partir d'horodatages `Instant`, plutôt que d'une méthode
/// de `rodio` : reste correcte quelle que soit l'API exacte de la version de `rodio` résolue par
/// Cargo, et fonctionne pour tous les formats (certains décodeurs n'exposent pas leur position).
struct TrackTiming {
    /// Horodatage du dernier (re)démarrage effectif ; `None` tant que la lecture est en pause.
    started_at: Option<Instant>,
    /// Temps déjà écoulé avant ce dernier démarrage (cumul des segments déjà joués).
    elapsed_before: Duration,
}

impl TrackTiming {
    fn playing_from_zero() -> Self {
        Self {
            started_at: Some(Instant::now()),
            elapsed_before: Duration::ZERO,
        }
    }

    fn stopped() -> Self {
        Self {
            started_at: None,
            elapsed_before: Duration::ZERO,
        }
    }

    fn position(&self) -> Duration {
        match self.started_at {
            Some(start) => self.elapsed_before + start.elapsed(),
            None => self.elapsed_before,
        }
    }

    fn pause(&mut self) {
        if let Some(start) = self.started_at.take() {
            self.elapsed_before += start.elapsed();
        }
    }

    fn resume(&mut self) {
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
    }

    fn seek_to(&mut self, position: Duration) {
        let was_running = self.started_at.is_some();
        self.elapsed_before = position;
        self.started_at = if was_running { Some(Instant::now()) } else { None };
    }
}

fn run(rx: Receiver<AudioCommand>, app: tauri::AppHandle) {
    let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
        Ok(pair) => pair,
        Err(e) => {
            emit_error(&app, format!("Périphérique audio indisponible : {e}"));
            return;
        }
    };

    let mut sink: Option<rodio::Sink> = None;
    let mut current_path: Option<String> = None;
    let mut timing = TrackTiming::stopped();

    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(AudioCommand::Load(path)) => match load(&stream_handle, &path) {
                Ok(new_sink) => {
                    sink = Some(new_sink);
                    current_path = Some(path.clone());
                    timing = TrackTiming::playing_from_zero();
                    let _ = app.emit("player-loaded", path);
                }
                Err(e) => emit_error(&app, format!("Lecture de « {path} » impossible : {e}")),
            },
            Ok(AudioCommand::Play) => {
                if let Some(s) = &sink {
                    s.play();
                    timing.resume();
                }
            }
            Ok(AudioCommand::Pause) => {
                if let Some(s) = &sink {
                    s.pause();
                    timing.pause();
                }
            }
            Ok(AudioCommand::Stop) => {
                if let Some(s) = sink.take() {
                    s.stop();
                }
                current_path = None;
                timing = TrackTiming::stopped();
            }
            Ok(AudioCommand::SetVolume(v)) => {
                if let Some(s) = &sink {
                    s.set_volume(v.clamp(0.0, 1.0));
                }
            }
            Ok(AudioCommand::Seek(pos)) => {
                if let Some(s) = &sink {
                    match s.try_seek(pos) {
                        Ok(()) => timing.seek_to(pos),
                        Err(e) => emit_error(&app, format!("Positionnement impossible : {e}")),
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        let ended = current_path.is_some() && sink.as_ref().map(|s| s.empty()).unwrap_or(false);
        let playing = sink.as_ref().map(|s| !s.is_paused()).unwrap_or(false);
        let _ = app.emit(
            "player-tick",
            PlayerTick {
                path: current_path.clone(),
                position_ms: timing.position().as_millis() as u64,
                playing,
                ended,
            },
        );
        if ended {
            sink = None;
            current_path = None;
            timing = TrackTiming::stopped();
        }
    }
}

/// Charge un fichier et démarre sa lecture immédiatement (un `Sink` neuf par piste : recréer un
/// `Sink` est bon marché et évite tout état résiduel de la piste précédente).
fn load(stream_handle: &rodio::OutputStreamHandle, path: &str) -> Result<rodio::Sink, String> {
    let file = std::fs::File::open(Path::new(path)).map_err(|e| e.to_string())?;
    let source =
        rodio::Decoder::new(std::io::BufReader::new(file)).map_err(|e| e.to_string())?;
    let sink = rodio::Sink::try_new(stream_handle).map_err(|e| e.to_string())?;
    sink.append(source);
    sink.play();
    Ok(sink)
}

fn emit_error(app: &tauri::AppHandle, message: String) {
    let _ = app.emit("player-error", PlayerError { message });
}

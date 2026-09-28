// Pas de console noire derrière la fenêtre en release sous Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;

use audio::AudioCommand;
use n7_core::cache;
use n7_core::library::LibraryIndex;
use n7_core::paths;
use n7_core::queue::{PlaybackQueue, RepeatMode};
use n7_core::scanner::{scan_sources, SourceScanOutcome};
use n7_core::settings::{AddOutcome, MusicSource, Settings};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use tauri::{Manager, State};

/// État partagé : dossier de données, réglages, file de lecture et canal vers le thread audio.
struct AppState {
    dir: PathBuf,
    settings: Mutex<Settings>,
    queue: Mutex<PlaybackQueue>,
    audio_tx: Sender<AudioCommand>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    data_dir: String,
    os: &'static str,
    arch: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AddResult {
    /// `added`, `duplicate` ou `nested`.
    outcome: &'static str,
    /// Complément lisible (nom de la source parente pour `nested`).
    detail: Option<String>,
    sources: Vec<MusicSource>,
}

const LOCK_ERROR: &str = "état interne verrouillé";

/// Applique `change` aux réglages sous verrou et renvoie son résultat avec une copie des réglages.
/// Le verrou est relâché avant tout `await` : il n'est jamais tenu pendant une opération disque.
fn with_settings<T>(
    state: &AppState,
    change: impl FnOnce(&mut Settings) -> T,
) -> Result<(T, Settings), String> {
    let mut guard = state
        .settings
        .lock()
        .map_err(|_| String::from(LOCK_ERROR))?;
    let settings: &mut Settings = &mut guard;
    let result = change(settings);
    Ok((result, guard.clone()))
}

/// Teste l'existence d'un dossier sur un thread d'arrière-plan : un partage réseau injoignable
/// peut bloquer plusieurs secondes et ne doit jamais figer l'interface.
async fn dir_exists(path: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || Path::new(&path).is_dir())
        .await
        .map_err(|e| e.to_string())
}

/// Écrit les réglages sur disque (thread d'arrière-plan).
async fn persist(dir: PathBuf, settings: Settings) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || settings.save(&dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("écriture des réglages impossible : {e}"))
}

#[tauri::command]
fn app_info(state: State<'_, AppState>) -> AppInfo {
    AppInfo {
        name: "n7Folder Player",
        version: env!("CARGO_PKG_VERSION"),
        data_dir: state.dir.display().to_string(),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

#[tauri::command]
fn get_sources(state: State<'_, AppState>) -> Result<Vec<MusicSource>, String> {
    let (sources, _) = with_settings(&state, |s| s.sources.clone())?;
    Ok(sources)
}

#[tauri::command]
async fn add_source(path: String, state: State<'_, AppState>) -> Result<AddResult, String> {
    if !dir_exists(path.clone()).await? {
        return Err(format!("dossier introuvable ou inaccessible : {path}"));
    }
    let (outcome, snapshot) = with_settings(&state, |s| s.add_source(&path))?;
    let (label, detail) = match outcome {
        AddOutcome::Added => ("added", None),
        AddOutcome::Duplicate => ("duplicate", None),
        AddOutcome::Nested { parent_label } => ("nested", Some(parent_label)),
    };
    if label == "added" {
        persist(state.dir.clone(), snapshot.clone()).await?;
    }
    Ok(AddResult {
        outcome: label,
        detail,
        sources: snapshot.sources,
    })
}

#[tauri::command]
async fn remove_source(id: String, state: State<'_, AppState>) -> Result<Vec<MusicSource>, String> {
    let (removed, snapshot) = with_settings(&state, |s| s.remove_source(&id))?;
    if removed {
        persist(state.dir.clone(), snapshot.clone()).await?;
    }
    Ok(snapshot.sources)
}

#[tauri::command]
async fn relink_source(
    id: String,
    new_path: String,
    state: State<'_, AppState>,
) -> Result<Vec<MusicSource>, String> {
    if !dir_exists(new_path.clone()).await? {
        return Err(format!("dossier introuvable ou inaccessible : {new_path}"));
    }
    let (result, snapshot) = with_settings(&state, |s| s.relink_source(&id, &new_path))?;
    result.map_err(|e| e.to_string())?;
    persist(state.dir.clone(), snapshot.clone()).await?;
    Ok(snapshot.sources)
}

/// Disponibilité d'une source (disque débranché, NAS éteint…). Un appel par source, en parallèle
/// côté interface : une source lente n'empêche pas les autres d'afficher leur état.
#[tauri::command]
async fn probe_source(id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let (path, _) = with_settings(&state, |s| {
        s.sources.iter().find(|src| src.id == id).map(|src| src.path.clone())
    })?;
    match path {
        Some(path) => dir_exists(path).await,
        None => Err(format!("source inconnue : {id}")),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtistLite {
    key: String,
    name: String,
    album_count: usize,
    track_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanReport {
    total_files: usize,
    elapsed_ms: u64,
    artist_count: usize,
    sources: Vec<SourceScanOutcome>,
    artists: Vec<ArtistLite>,
}

/// Scanne toutes les sources, construit l'index artistes/albums/pistes, l'enregistre dans le
/// cache local (`library.json`) et renvoie un résumé léger à l'interface (pas les pistes une par
/// une : sur 50 000 fichiers ce serait un aller-retour JSON inutilement volumineux pour cette
/// itération, qui n'affiche encore que des compteurs).
#[tauri::command]
async fn scan_library(state: State<'_, AppState>) -> Result<ScanReport, String> {
    let (sources, _) = with_settings(&state, |s| s.sources.clone())?;
    let dir = state.dir.clone();

    let summary = tauri::async_runtime::spawn_blocking(move || scan_sources(&sources))
        .await
        .map_err(|e| e.to_string())?;

    let total_files = summary.total_files;
    let elapsed_ms = summary.elapsed_ms;

    // Une seule copie des pistes : l'original alimente le cache disque, la copie alimente l'index
    // en mémoire. Fusionner les deux en un seul passage est une optimisation pour l'itération 5.
    let all_tracks: Vec<_> = summary.sources.iter().flat_map(|s| s.tracks.clone()).collect();
    let tracks_for_cache = all_tracks.clone();

    let mut index = LibraryIndex::new();
    index.add_tracks(all_tracks);
    let artists = index.snapshot();
    let artist_count = artists.len();

    let dir_for_cache = dir.clone();
    tauri::async_runtime::spawn_blocking(move || cache::save(&dir_for_cache, &tracks_for_cache))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("écriture du cache impossible : {e}"))?;

    Ok(ScanReport {
        total_files,
        elapsed_ms,
        artist_count,
        sources: summary.sources,
        artists: artists
            .into_iter()
            .map(|a| {
                // Calculés avant le literal ci-dessous : une fois `a.key`/`a.name` déplacés hors
                // de `a`, on ne peut plus emprunter `a` en entier pour appeler `a.track_count()`.
                let album_count = a.albums.len();
                let track_count = a.track_count();
                ArtistLite {
                    key: a.key,
                    name: a.name,
                    album_count,
                    track_count,
                }
            })
            .collect(),
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrackLite {
    path: String,
    title: String,
    artist: String,
    album: String,
    track_number: Option<u32>,
}

/// Pistes d'un artiste, lues depuis le cache local (pas un nouveau scan) : sert à peupler la file
/// de lecture pour tester le moteur audio en attendant la vraie interface n7player (itération 4).
#[tauri::command]
async fn list_tracks_for_artist(
    artist_key: String,
    state: State<'_, AppState>,
) -> Result<Vec<TrackLite>, String> {
    let dir = state.dir.clone();
    let tracks = tauri::async_runtime::spawn_blocking(move || cache::load(&dir))
        .await
        .map_err(|e| e.to_string())?;

    let mut out: Vec<TrackLite> = tracks
        .into_iter()
        .filter(|t| t.folder.artist_key == artist_key)
        .map(|t| TrackLite {
            path: t.path,
            title: t.title,
            artist: t.folder.artist,
            album: t.folder.album,
            track_number: t.track_number,
        })
        .collect();
    out.sort_by(|a, b| a.album.cmp(&b.album).then(a.track_number.cmp(&b.track_number)));
    Ok(out)
}

const AUDIO_UNAVAILABLE: &str = "moteur audio indisponible";

fn send_audio(state: &AppState, command: AudioCommand) -> Result<(), String> {
    state
        .audio_tx
        .send(command)
        .map_err(|_| AUDIO_UNAVAILABLE.to_string())
}

/// Envoie `Load` pour la piste actuellement pointée par la file, s'il y en a une.
fn load_current(state: &AppState, queue: &PlaybackQueue) -> Result<(), String> {
    if let Some(path) = queue.current() {
        send_audio(state, AudioCommand::Load(path.to_string()))?;
    }
    Ok(())
}

#[tauri::command]
fn player_play(state: State<'_, AppState>) -> Result<(), String> {
    send_audio(&state, AudioCommand::Play)
}

#[tauri::command]
fn player_pause(state: State<'_, AppState>) -> Result<(), String> {
    send_audio(&state, AudioCommand::Pause)
}

#[tauri::command]
fn player_stop(state: State<'_, AppState>) -> Result<(), String> {
    send_audio(&state, AudioCommand::Stop)
}

#[tauri::command]
fn player_set_volume(volume: f32, state: State<'_, AppState>) -> Result<(), String> {
    send_audio(&state, AudioCommand::SetVolume(volume.clamp(0.0, 1.0)))
}

#[tauri::command]
fn player_seek(seconds: f64, state: State<'_, AppState>) -> Result<(), String> {
    let position = std::time::Duration::from_secs_f64(seconds.max(0.0));
    send_audio(&state, AudioCommand::Seek(position))
}

/// Remplace la file de lecture (par exemple : toutes les pistes d'un artiste, dans l'ordre) et
/// démarre immédiatement la première piste.
#[tauri::command]
fn player_set_queue(track_ids: Vec<String>, state: State<'_, AppState>) -> Result<(), String> {
    let mut queue = state.queue.lock().map_err(|_| LOCK_ERROR.to_string())?;
    queue.set_order(track_ids);
    load_current(&state, &queue)
}

#[tauri::command]
fn player_jump_to(track_id: String, state: State<'_, AppState>) -> Result<bool, String> {
    let mut queue = state.queue.lock().map_err(|_| LOCK_ERROR.to_string())?;
    let found = queue.jump_to(&track_id);
    if found {
        load_current(&state, &queue)?;
    }
    Ok(found)
}

#[tauri::command]
fn player_next(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let mut queue = state.queue.lock().map_err(|_| LOCK_ERROR.to_string())?;
    let next = queue.next().map(str::to_string);
    if next.is_some() {
        load_current(&state, &queue)?;
    }
    Ok(next)
}

#[tauri::command]
fn player_previous(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let mut queue = state.queue.lock().map_err(|_| LOCK_ERROR.to_string())?;
    let previous = queue.previous().map(str::to_string);
    if previous.is_some() {
        load_current(&state, &queue)?;
    }
    Ok(previous)
}

#[tauri::command]
fn player_set_shuffle(on: bool, state: State<'_, AppState>) -> Result<(), String> {
    let mut queue = state.queue.lock().map_err(|_| LOCK_ERROR.to_string())?;
    queue.set_shuffle(on);
    Ok(())
}

#[tauri::command]
fn player_set_repeat(mode: RepeatMode, state: State<'_, AppState>) -> Result<(), String> {
    let mut queue = state.queue.lock().map_err(|_| LOCK_ERROR.to_string())?;
    queue.set_repeat(mode);
    Ok(())
}

/// Effet Mica (Windows 11) avec repli acrylique (Windows 10 / builds antérieures).
#[cfg(target_os = "windows")]
fn apply_backdrop(app: &tauri::App) {
    if let Some(window) = app.get_webview_window("main") {
        if window_vibrancy::apply_mica(&window, Some(true)).is_err() {
            let _ = window_vibrancy::apply_acrylic(&window, Some((16, 20, 32, 200)));
        }
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = paths::data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let settings = Settings::load(&dir);
            let audio_tx = audio::spawn(app.handle().clone());
            app.manage(AppState {
                dir,
                settings: Mutex::new(settings),
                queue: Mutex::new(PlaybackQueue::new(Vec::new())),
                audio_tx,
            });
            #[cfg(target_os = "windows")]
            apply_backdrop(app);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_sources,
            add_source,
            remove_source,
            relink_source,
            probe_source,
            scan_library,
            list_tracks_for_artist,
            player_play,
            player_pause,
            player_stop,
            player_set_volume,
            player_seek,
            player_set_queue,
            player_jump_to,
            player_next,
            player_previous,
            player_set_shuffle,
            player_set_repeat
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de n7Folder Player");
}

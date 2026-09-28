//! Scanner de fichiers : parcourt les dossiers sources et produit des [`TrackEntry`].
//!
//! Port de `FileScanner.kt` (application Android, où l'accès se fait via le Storage Access
//! Framework) vers un système de fichiers classique (`std::fs`). Les règles de robustesse restent
//! les mêmes :
//! - parcours **itératif** avec une pile explicite (jamais de récursion : pas de dépassement de
//!   pile sur une arborescence très profonde) ;
//! - **une source par thread natif** : un NAS injoignable ne bloque que sa propre source, jamais
//!   les autres, et jamais l'interface ;
//! - profondeur maximale, dossiers déjà visités, fichiers vides et dossiers système ignorés ;
//! - un dossier illisible n'arrête jamais le scan (compté dans `skipped`) ; une racine illisible
//!   marque la source `unavailable` pour proposer un relink, sans faire échouer les autres sources.

use crate::library::TrackEntry;
use crate::parse_path::{self, PathNormalizer};
use crate::settings::MusicSource;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_DEPTH: u32 = 20;

const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "aac", "m4a", "ogg", "oga", "opus", "wav"];

// Comparaison en minuscules.
const IGNORED_DIRS: &[&str] = &[
    "@eadir",
    "$recycle.bin",
    "system volume information",
    "lost+found",
    "#recycle",
    "@recycle",
];

/// Compteurs d'une source scannée.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStats {
    pub files: usize,
    pub dirs: usize,
    pub skipped: usize,
}

/// Résultat du scan d'une source : ses pistes et ses compteurs, ou `unavailable` si sa racine
/// n'a pas pu être lue du tout (disque débranché, chemin supprimé, permission refusée).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceScanOutcome {
    pub source_id: String,
    pub label: String,
    pub unavailable: bool,
    pub stats: ScanStats,
    #[serde(skip)]
    pub tracks: Vec<TrackEntry>,
}

/// Résultat global d'un scan de toutes les sources.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummary {
    pub total_files: usize,
    /// En millisecondes ; `u64` plutôt que le `u128` natif de [`std::time::Duration`] pour rester
    /// dans les entiers que JSON représente sans ambiguïté (un scan ne dure jamais 2^64 ms).
    pub elapsed_ms: u64,
    pub sources: Vec<SourceScanOutcome>,
}

impl ScanSummary {
    /// Toutes les pistes de toutes les sources, fusionnées (consomme le résumé).
    pub fn into_tracks(self) -> Vec<TrackEntry> {
        let mut out = Vec::new();
        for source in self.sources {
            out.extend(source.tracks);
        }
        out
    }
}

/// Scanne toutes les sources en parallèle (un thread natif par source) et fusionne les résultats.
///
/// Un thread qui panique (bug interne) ne fait pas planter les autres : sa source est simplement
/// absente du résultat plutôt que de perdre tout le scan.
pub fn scan_sources(sources: &[MusicSource]) -> ScanSummary {
    let start = std::time::Instant::now();
    let seen: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let handles: Vec<_> = sources
        .iter()
        .cloned()
        .map(|source| {
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || scan_source(&source, &seen))
        })
        .collect();

    let mut outcomes = Vec::with_capacity(handles.len());
    for handle in handles {
        if let Ok(outcome) = handle.join() {
            outcomes.push(outcome);
        }
    }

    let total_files = outcomes.iter().map(|o| o.stats.files).sum();
    ScanSummary {
        total_files,
        elapsed_ms: start.elapsed().as_millis() as u64,
        sources: outcomes,
    }
}

struct DirFrame {
    path: PathBuf,
    /// Segments utilisés par [`PathNormalizer`] (peut inclure le nom de la racine si celle-ci
    /// est elle-même un dossier d'artiste).
    segments: Vec<String>,
    /// Chemin relatif à la racine de la source, pour un futur explorateur (itération 4).
    rel_path: Vec<String>,
    depth: u32,
}

/// Scanne une seule source. Ne panique jamais : toute erreur d'E/S dégrade en `skipped` ou en
/// source `unavailable`, jamais en `Result` propagé (un dossier illisible parmi 50 000 ne doit
/// pas interrompre le scan).
fn scan_source(source: &MusicSource, seen: &Mutex<HashSet<String>>) -> SourceScanOutcome {
    let root = Path::new(&source.path);
    if !root.is_dir() {
        return SourceScanOutcome {
            source_id: source.id.clone(),
            label: source.label.clone(),
            unavailable: true,
            stats: ScanStats::default(),
            tracks: Vec::new(),
        };
    }

    let known_keys = collect_known_artist_keys(root, source.root_is_artist, &source.label);
    let normalizer = PathNormalizer::new(known_keys);

    let root_segments: Vec<String> = if source.root_is_artist && !source.label.is_empty() {
        vec![source.label.clone()]
    } else {
        Vec::new()
    };

    let mut stack = vec![DirFrame {
        path: root.to_path_buf(),
        segments: root_segments,
        rel_path: Vec::new(),
        depth: 0,
    }];

    let mut tracks = Vec::new();
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut skipped = 0usize;

    while let Some(frame) = stack.pop() {
        let entries = match std::fs::read_dir(&frame.path) {
            Ok(e) => e,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        dirs += 1;

        let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
        let mut audio: Vec<(String, PathBuf, u64)> = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            if file_type.is_dir() {
                if !is_ignored_dir(&name) {
                    subdirs.push((name, entry.path()));
                }
                continue;
            }
            if !file_type.is_file() {
                continue; // liens symboliques non résolus, périphériques, etc. : ignorés
            }
            if is_audio_file(&name) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if size == 0 {
                    skipped += 1; // fichier vide : corrompu, inutile de le proposer à la lecture
                } else {
                    audio.push((name, entry.path(), size));
                }
            }
        }

        subdirs.sort_by(|a, b| parse_path::natural_compare(&a.0, &b.0));
        audio.sort_by(|a, b| parse_path::natural_compare(&a.0, &b.0));

        if !audio.is_empty() {
            let parsed = normalizer.parse_folder(&frame.segments);
            let file_names: Vec<String> = audio.iter().map(|(name, _, _)| name.clone()).collect();
            let space_numbered = parse_path::looks_space_numbered(&file_names);

            for (name, path, size) in &audio {
                let key = unique_key(path);
                let is_new = {
                    let mut guard = seen.lock().unwrap_or_else(|poison| poison.into_inner());
                    guard.insert(key)
                };
                if !is_new {
                    skipped += 1; // même fichier atteint par deux sources imbriquées
                    continue;
                }

                let track_name = normalizer.parse_track_name(name, &parsed.artist_key, space_numbered);
                let disc = track_name.disc.or(parsed.disc);
                tracks.push(TrackEntry {
                    source_id: source.id.clone(),
                    path: path.to_string_lossy().into_owned(),
                    file_name: name.clone(),
                    title: track_name.title,
                    track_number: track_name.track,
                    disc_number: disc,
                    folder: parsed.clone(),
                    dir_path: frame.rel_path.clone(),
                    size_bytes: *size,
                });
                files += 1;
            }
        }

        // Sous-dossiers empilés en ordre inverse pour être dépilés de A à Z.
        if frame.depth < MAX_DEPTH {
            for (name, path) in subdirs.into_iter().rev() {
                let mut segments = frame.segments.clone();
                segments.push(name.clone());
                let mut rel_path = frame.rel_path.clone();
                rel_path.push(name);
                stack.push(DirFrame {
                    path,
                    segments,
                    rel_path,
                    depth: frame.depth + 1,
                });
            }
        }
    }

    SourceScanOutcome {
        source_id: source.id.clone(),
        label: source.label.clone(),
        unavailable: false,
        stats: ScanStats { files, dirs, skipped },
        tracks,
    }
}

/// Premier niveau (jusqu'à 3 niveaux si des dossiers génériques "Musique/", "FLAC/" s'intercalent) :
/// clés des dossiers artistes, pour la fusion prudente "A & B" -> "A" (voir [`PathNormalizer`]).
/// Un échec de lecture à ce stade n'est jamais fatal : la fusion "A & B" sera simplement plus
/// prudente (les deux artistes resteront groupés), pas un blocage du scan.
fn collect_known_artist_keys(root: &Path, root_is_artist: bool, root_label: &str) -> HashSet<String> {
    let mut keys = HashSet::new();
    if root_is_artist && !root_label.is_empty() {
        keys.insert(parse_path::artist_key_hint(root_label));
        return keys;
    }

    let mut level: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut depth = 0;
    while !level.is_empty() && depth < 3 {
        let mut next = Vec::new();
        for dir in &level {
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') || is_ignored_dir(&name) {
                    continue;
                }
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if !is_dir {
                    continue;
                }
                if parse_path::is_generic_container(&name) {
                    next.push(entry.path());
                } else {
                    keys.insert(parse_path::artist_key_hint(&name));
                }
            }
        }
        level = next;
        depth += 1;
    }
    keys
}

fn extension_of(name: &str) -> String {
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => name[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

fn is_audio_file(name: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&extension_of(name).as_str())
}

fn is_ignored_dir(name: &str) -> bool {
    IGNORED_DIRS.contains(&name.to_lowercase().as_str())
}

/// Clé de déduplication rapide (comparaison de chaînes, sans appel système `canonicalize` par
/// fichier : sur 50 000 pistes, l'appel système supplémentaire coûterait cher pour un cas —
/// deux sources configurées qui se chevauchent — que [`crate::settings::Settings::add_source`]
/// empêche déjà à la création.
fn unique_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("n7scan-{tag}-{}-{nanos}", std::process::id()))
    }

    fn write_file(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn source(id: &str, path: &Path) -> MusicSource {
        MusicSource {
            id: id.to_string(),
            path: path.to_string_lossy().into_owned(),
            label: path.file_name().unwrap().to_string_lossy().into_owned(),
            root_is_artist: false,
        }
    }

    #[test]
    fn scans_nested_artist_album_disc_layout() {
        let root = temp_dir("basic");
        write_file(&root.join("Saez").join("Debbie").join("01 - Fifty Sixty.mp3"), b"donnees-audio");
        write_file(&root.join("Saez").join("Debbie").join("02 - Jeune et con.mp3"), b"donnees-audio");
        write_file(&root.join("Pink Floyd").join("The Wall").join("CD1").join("01.flac"), b"donnees-audio");
        write_file(&root.join("Pink Floyd").join("The Wall").join("CD2").join("01.flac"), b"donnees-audio");

        let outcome = scan_source(&source("s1", &root), &Mutex::new(HashSet::new()));

        assert!(!outcome.unavailable);
        assert_eq!(outcome.stats.files, 4);
        assert_eq!(outcome.tracks.len(), 4);

        let saez_track = outcome
            .tracks
            .iter()
            .find(|t| t.file_name == "01 - Fifty Sixty.mp3")
            .expect("piste Saez attendue");
        assert_eq!(saez_track.folder.artist, "Saez");
        assert_eq!(saez_track.folder.album, "Debbie");
        assert_eq!(saez_track.track_number, Some(1));

        let wall_cd1 = outcome
            .tracks
            .iter()
            .find(|t| t.dir_path.last().map(String::as_str) == Some("CD1"))
            .expect("piste CD1 attendue");
        assert_eq!(wall_cd1.folder.artist, "Pink Floyd");
        assert_eq!(wall_cd1.folder.album, "The Wall");
        assert_eq!(wall_cd1.disc_number, Some(1));

        let wall_cd2 = outcome
            .tracks
            .iter()
            .find(|t| t.dir_path.last().map(String::as_str) == Some("CD2"))
            .expect("piste CD2 attendue");
        assert_eq!(wall_cd2.disc_number, Some(2));
        assert_eq!(wall_cd2.folder.album_id(), wall_cd1.folder.album_id());

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ignores_hidden_files_system_dirs_and_empty_files() {
        let root = temp_dir("ignored");
        write_file(&root.join("Artist").join("Album").join("01 - Titre.mp3"), b"donnees-audio");
        write_file(&root.join("Artist").join("Album").join(".hidden.mp3"), b"donnees-audio");
        write_file(&root.join("Artist").join("Album").join("vide.mp3"), b"");
        write_file(&root.join("$RECYCLE.BIN").join("Ghost").join("01.mp3"), b"donnees-audio");

        let outcome = scan_source(&source("s1", &root), &Mutex::new(HashSet::new()));

        assert_eq!(outcome.tracks.len(), 1);
        assert_eq!(outcome.tracks[0].file_name, "01 - Titre.mp3");
        // Seul le fichier vide est compté comme « ignoré » : les fichiers cachés et les dossiers
        // système sont écartés silencieusement, sans être signalés à l'utilisateur.
        assert_eq!(outcome.stats.skipped, 1);

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unavailable_source_is_reported_without_stopping_other_sources() {
        let missing = temp_dir("missing-does-not-exist");
        let outcome = scan_source(&source("gone", &missing), &Mutex::new(HashSet::new()));
        assert!(outcome.unavailable);
        assert_eq!(outcome.tracks.len(), 0);
    }

    #[test]
    fn scan_sources_runs_each_source_on_its_own_thread_and_merges_results() {
        let root_a = temp_dir("multi-a");
        let root_b = temp_dir("multi-b");
        write_file(&root_a.join("Artist A").join("Album A").join("01.mp3"), b"donnees-audio");
        write_file(&root_b.join("Artist B").join("Album B").join("01.mp3"), b"donnees-audio");

        let summary = scan_sources(&[source("a", &root_a), source("b", &root_b)]);

        assert_eq!(summary.total_files, 2);
        assert_eq!(summary.sources.len(), 2);
        assert!(summary.sources.iter().all(|s| !s.unavailable));

        fs::remove_dir_all(&root_a).ok();
        fs::remove_dir_all(&root_b).ok();
    }

    #[test]
    fn generic_container_and_format_folders_do_not_become_artists() {
        let root = temp_dir("generic");
        write_file(&root.join("Musique").join("FLAC").join("Daft Punk").join("Discovery").join("01.flac"), b"donnees-audio");

        let outcome = scan_source(&source("s1", &root), &Mutex::new(HashSet::new()));

        assert_eq!(outcome.tracks.len(), 1);
        assert_eq!(outcome.tracks[0].folder.artist, "Daft Punk");
        assert_eq!(outcome.tracks[0].folder.album, "Discovery");

        fs::remove_dir_all(&root).ok();
    }
}

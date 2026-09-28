//! Cache local de la bibliothèque indexée, dans `%APPDATA%\n7FolderPlayer\library.json`.
//!
//! Même robustesse que [`crate::settings`] : écriture atomique (fichier temporaire puis
//! renommage) et chargement qui n'échoue jamais (fichier absent ou illisible -> bibliothèque
//! vide, fichier corrompu mis de côté en `.corrupt` plutôt que perdu silencieusement).

use crate::library::TrackEntry;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

pub const LIBRARY_CACHE_FILE: &str = "library.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct CachePayload {
    #[serde(default)]
    tracks: Vec<TrackEntry>,
}

/// Enregistre l'intégralité des pistes indexées (écrasant le cache précédent).
pub fn save(dir: &Path, tracks: &[TrackEntry]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let payload = CachePayload {
        tracks: tracks.to_vec(),
    };
    let json = serde_json::to_vec(&payload).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{LIBRARY_CACHE_FILE}.tmp"));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, dir.join(LIBRARY_CACHE_FILE))
}

/// Charge le cache. Ne renvoie jamais d'erreur : au pire, une liste vide.
pub fn load(dir: &Path) -> Vec<TrackEntry> {
    let file = dir.join(LIBRARY_CACHE_FILE);
    let bytes = match fs::read(&file) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    match serde_json::from_slice::<CachePayload>(&bytes) {
        Ok(payload) => payload.tracks,
        Err(_) => {
            let _ = fs::rename(&file, dir.join(format!("{LIBRARY_CACHE_FILE}.corrupt")));
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_path::ParsedFolder;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("n7cache-{tag}-{}-{nanos}", std::process::id()))
    }

    fn sample_track() -> TrackEntry {
        TrackEntry {
            source_id: "s1".to_string(),
            path: "D:\\Musique\\Saez\\Debbie\\01.mp3".to_string(),
            file_name: "01.mp3".to_string(),
            title: "Fifty Sixty".to_string(),
            track_number: Some(1),
            disc_number: None,
            folder: ParsedFolder {
                artist: "Saez".to_string(),
                artist_key: "saez".to_string(),
                album: "Debbie".to_string(),
                album_key: "debbie".to_string(),
                year: None,
                disc: None,
            },
            dir_path: vec!["Saez".to_string(), "Debbie".to_string()],
            size_bytes: 4_200_000,
        }
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = temp_dir("roundtrip");
        let tracks = vec![sample_track()];
        save(&dir, &tracks).unwrap();
        assert!(!dir.join(format!("{LIBRARY_CACHE_FILE}.tmp")).exists());
        let loaded = load(&dir);
        assert_eq!(loaded, tracks);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_or_corrupt_cache_gives_an_empty_library_without_erroring() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir), Vec::new());

        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(LIBRARY_CACHE_FILE), b"{ pas du json").unwrap();
        assert_eq!(load(&dir), Vec::new());
        assert!(dir.join(format!("{LIBRARY_CACHE_FILE}.corrupt")).exists());
        fs::remove_dir_all(&dir).ok();
    }
}

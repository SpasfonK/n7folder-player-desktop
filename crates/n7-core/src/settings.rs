//! Dossiers sources de la bibliothèque, persistés dans `%APPDATA%\n7FolderPlayer\settings.json`.
//!
//! Règles héritées de Musico / n7Folder Player Android :
//! * l'ajout est **cumulatif** (plusieurs dossiers, disques externes, partages UNC) sans écrasement ;
//! * l'identifiant d'une source est **stable** : il survit à un « relink » (lettre de lecteur
//!   changée, NAS déplacé) pour que l'index déjà construit reste valable ;
//! * la comparaison des chemins ignore la casse et le sens des séparateurs (Windows).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Nom du fichier de réglages dans le dossier de données.
pub const SETTINGS_FILE: &str = "settings.json";

/// Un dossier racine choisi par l'utilisateur.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MusicSource {
    /// Identifiant stable (survit à un relink).
    pub id: String,
    /// Chemin tel que choisi : `D:\Musique`, `\\NAS\Musique`…
    pub path: String,
    /// Nom affiché.
    pub label: String,
    /// Vrai si ce dossier EST un dossier artiste (ses sous-dossiers sont alors des albums).
    #[serde(default)]
    pub root_is_artist: bool,
}

/// Réglages persistés.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub sources: Vec<MusicSource>,
}

/// Résultat d'un ajout de source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    /// La source a été ajoutée.
    Added,
    /// Ce chemin est déjà une source.
    Duplicate,
    /// Ce chemin est situé à l'intérieur d'une source existante (déjà couvert).
    Nested { parent_label: String },
}

/// Échec d'un relink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelinkError {
    /// Aucune source avec cet identifiant.
    NotFound,
    /// Le nouveau chemin est déjà utilisé par une autre source.
    Conflict { other_label: String },
}

impl fmt::Display for RelinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RelinkError::NotFound => write!(f, "source inconnue"),
            RelinkError::Conflict { other_label } => {
                write!(f, "ce dossier est déjà utilisé par la source « {other_label} »")
            }
        }
    }
}

/// Clé de comparaison d'un chemin : séparateurs unifiés en `\`, séparateur final retiré, minuscules.
///
/// `D:\Musique`, `d:/musique/` et `D:\MUSIQUE\` donnent la même clé.
pub fn source_key(path: &str) -> String {
    let unified: String = path
        .trim()
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();
    let trimmed = unified.trim_end_matches('\\');
    if trimmed.is_empty() {
        unified.to_lowercase()
    } else {
        trimmed.to_lowercase()
    }
}

/// Identifiant déterministe (FNV-1a 64 bits de la clé du chemin), en hexadécimal.
pub fn source_id(path: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source_key(path).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Nom affiché d'un chemin : son dernier composant (`D:\Musique\Rock` → `Rock`, `D:\` → `D:`).
pub fn label_for(path: &str) -> String {
    match path.rsplit(['\\', '/']).find(|part| !part.is_empty()) {
        Some(part) => part.to_string(),
        None => path.trim().to_string(),
    }
}

impl Settings {
    /// Ajoute un dossier source. Ne remplace jamais une source existante.
    pub fn add_source(&mut self, path: &str) -> AddOutcome {
        let key = source_key(path);
        if self.sources.iter().any(|s| source_key(&s.path) == key) {
            return AddOutcome::Duplicate;
        }
        let parent = self
            .sources
            .iter()
            .find(|s| key.starts_with(&format!("{}\\", source_key(&s.path))));
        if let Some(parent) = parent {
            return AddOutcome::Nested {
                parent_label: parent.label.clone(),
            };
        }
        let id = self.unique_id(source_id(path));
        self.sources.push(MusicSource {
            id,
            path: path.trim().to_string(),
            label: label_for(path),
            root_is_artist: false,
        });
        AddOutcome::Added
    }

    /// Retire une source ; renvoie `false` si l'identifiant est inconnu.
    pub fn remove_source(&mut self, id: &str) -> bool {
        let before = self.sources.len();
        self.sources.retain(|s| s.id != id);
        self.sources.len() != before
    }

    /// Réaligne une source sur un nouveau chemin en conservant son identifiant.
    pub fn relink_source(&mut self, id: &str, new_path: &str) -> Result<(), RelinkError> {
        let index = self
            .sources
            .iter()
            .position(|s| s.id == id)
            .ok_or(RelinkError::NotFound)?;
        let key = source_key(new_path);
        let other = self
            .sources
            .iter()
            .enumerate()
            .find(|(i, s)| *i != index && source_key(&s.path) == key)
            .map(|(_, s)| s.label.clone());
        if let Some(other_label) = other {
            return Err(RelinkError::Conflict { other_label });
        }
        let source = &mut self.sources[index];
        let label_was_automatic = source.label == label_for(&source.path);
        source.path = new_path.trim().to_string();
        if label_was_automatic {
            source.label = label_for(new_path);
        }
        Ok(())
    }

    /// Identifiant libre : `base`, sinon `base-2`, `base-3`… (cas d'un chemin réutilisé après relink).
    fn unique_id(&self, base: String) -> String {
        if !self.sources.iter().any(|s| s.id == base) {
            return base;
        }
        let mut n: u32 = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !self.sources.iter().any(|s| s.id == candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Charge les réglages. N'échoue jamais : fichier absent → réglages vides ; fichier illisible
    /// → il est mis de côté (`settings.json.corrupt`) et des réglages vides sont renvoyés.
    pub fn load(dir: &Path) -> Settings {
        let file = dir.join(SETTINGS_FILE);
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(_) => return Settings::default(),
        };
        let payload = bytes.strip_prefix(&[0xEF_u8, 0xBB, 0xBF]).unwrap_or(&bytes);
        match serde_json::from_slice::<Settings>(payload) {
            Ok(settings) => settings,
            Err(_) => {
                let _ = fs::rename(&file, dir.join(format!("{SETTINGS_FILE}.corrupt")));
                Settings::default()
            }
        }
    }

    /// Enregistre les réglages de façon atomique (fichier temporaire + renommage) : une coupure
    /// pendant l'écriture ne peut pas laisser un fichier tronqué.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let tmp = dir.join(format!("{SETTINGS_FILE}.tmp"));
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, dir.join(SETTINGS_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("n7core-{tag}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn key_and_id_ignore_case_separators_and_trailing_slash() {
        assert_eq!(source_key("D:\\Musique"), "d:\\musique");
        assert_eq!(source_key("d:/musique/"), "d:\\musique");
        assert_eq!(source_id("D:\\Musique"), source_id("d:/MUSIQUE/"));
        assert_ne!(source_id("D:\\Musique"), source_id("E:\\Musique"));
        assert_eq!(source_key("D:\\"), "d:");
        assert_eq!(source_key("\\\\NAS\\Musique\\"), "\\\\nas\\musique");
    }

    #[test]
    fn labels_are_last_component() {
        assert_eq!(label_for("D:\\Musique\\Rock"), "Rock");
        assert_eq!(label_for("D:\\"), "D:");
        assert_eq!(label_for("\\\\nas\\musique\\"), "musique");
        assert_eq!(label_for("/mnt/music/"), "music");
    }

    #[test]
    fn add_is_cumulative_and_never_overwrites() {
        let mut s = Settings::default();
        assert_eq!(s.add_source("D:\\FLAC"), AddOutcome::Added);
        assert_eq!(s.add_source("E:\\MP3"), AddOutcome::Added);
        assert_eq!(s.add_source("\\\\NAS\\Musique"), AddOutcome::Added);
        assert_eq!(s.add_source("d:/flac/"), AddOutcome::Duplicate);
        assert_eq!(s.sources.len(), 3);
        assert_eq!(s.sources[0].label, "FLAC");
        assert_eq!(s.sources[2].label, "Musique");
    }

    #[test]
    fn nested_folder_is_reported_but_sibling_prefix_is_not() {
        let mut s = Settings::default();
        s.add_source("D:\\Musique");
        assert_eq!(
            s.add_source("D:\\Musique\\Saez"),
            AddOutcome::Nested {
                parent_label: "Musique".to_string()
            }
        );
        assert_eq!(s.add_source("D:\\Musique2"), AddOutcome::Added);
        assert_eq!(s.sources.len(), 2);
    }

    #[test]
    fn a_parent_can_be_added_after_its_child() {
        let mut s = Settings::default();
        assert_eq!(s.add_source("D:\\Musique\\Saez"), AddOutcome::Added);
        assert_eq!(s.add_source("D:\\Musique"), AddOutcome::Added);
        assert_eq!(s.sources.len(), 2);
    }

    #[test]
    fn remove_by_id() {
        let mut s = Settings::default();
        s.add_source("D:\\FLAC");
        let id = s.sources[0].id.clone();
        assert!(!s.remove_source("inconnu"));
        assert!(s.remove_source(&id));
        assert!(s.sources.is_empty());
    }

    #[test]
    fn relink_keeps_id_and_updates_automatic_label() {
        let mut s = Settings::default();
        s.add_source("E:\\Musique");
        let id = s.sources[0].id.clone();
        assert_eq!(s.relink_source(&id, "F:\\Musique-usb"), Ok(()));
        assert_eq!(s.sources[0].id, id);
        assert_eq!(s.sources[0].path, "F:\\Musique-usb");
        assert_eq!(s.sources[0].label, "Musique-usb");
    }

    #[test]
    fn relink_errors() {
        let mut s = Settings::default();
        s.add_source("D:\\A");
        s.add_source("D:\\B");
        let id_a = s.sources[0].id.clone();
        assert_eq!(s.relink_source("inconnu", "D:\\C"), Err(RelinkError::NotFound));
        assert_eq!(
            s.relink_source(&id_a, "d:/b/"),
            Err(RelinkError::Conflict {
                other_label: "B".to_string()
            })
        );
        assert_eq!(s.sources[0].path, "D:\\A");
    }

    #[test]
    fn reusing_an_old_path_after_relink_gets_a_fresh_id() {
        let mut s = Settings::default();
        s.add_source("E:\\Musique");
        let first = s.sources[0].id.clone();
        s.relink_source(&first, "F:\\Musique").unwrap();
        assert_eq!(s.add_source("E:\\Musique"), AddOutcome::Added);
        assert_ne!(s.sources[1].id, first);
        assert_eq!(s.sources[1].id, format!("{first}-2"));
    }

    #[test]
    fn save_then_load_roundtrip_uses_camel_case() {
        let dir = temp_dir("roundtrip");
        let mut s = Settings::default();
        s.add_source("D:\\FLAC");
        s.add_source("\\\\NAS\\Musique");
        s.save(&dir).unwrap();
        let raw = fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(raw.contains("rootIsArtist"));
        assert!(!dir.join(format!("{SETTINGS_FILE}.tmp")).exists());
        assert_eq!(Settings::load(&dir), s);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_handles_missing_bom_and_corrupt_files() {
        let dir = temp_dir("load");
        assert_eq!(Settings::load(&dir), Settings::default());

        fs::create_dir_all(&dir).unwrap();
        let mut with_bom = vec![0xEF, 0xBB, 0xBF];
        with_bom.extend_from_slice(br#"{"sources":[{"id":"a","path":"D:\\FLAC","label":"FLAC"}]}"#);
        fs::write(dir.join(SETTINGS_FILE), with_bom).unwrap();
        let loaded = Settings::load(&dir);
        assert_eq!(loaded.sources.len(), 1);
        assert!(!loaded.sources[0].root_is_artist);

        fs::write(dir.join(SETTINGS_FILE), b"{ pas du json").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        assert!(dir.join(format!("{SETTINGS_FILE}.corrupt")).exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}

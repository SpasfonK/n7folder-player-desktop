//! Modèle de données de la bibliothèque : fusionne les pistes détectées par le [`crate::scanner`]
//! en artistes -> albums -> pistes, prêts pour l'interface. Port de `LibraryModels.kt` et
//! `LibraryIndex.kt` (application Android n7Folder Player).

use crate::parse_path::{display_score, natural_compare, ParsedFolder};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Une piste audio indexée. `folder` est partagé par toutes les pistes du même dossier (même
/// valeur clonée, pas de recalcul du parseur par piste).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackEntry {
    pub source_id: String,
    /// Chemin absolu complet du fichier sur le disque.
    pub path: String,
    pub file_name: String,
    pub title: String,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub folder: ParsedFolder,
    /// Chemin du dossier relatif à la racine de la source (pour l'explorateur, itération 4).
    pub dir_path: Vec<String>,
    pub size_bytes: u64,
}

/// Identité d'un album : l'artiste fait partie de la clé (deux "Greatest Hits" ne fusionnent pas).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumKey {
    pub artist_key: String,
    pub album_key: String,
}

/// Vue immuable d'un album pour l'interface ; `tracks` est déjà dans l'ordre de lecture.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumSummary {
    pub key: AlbumKey,
    pub name: String,
    pub year: Option<i32>,
    pub disc_count: usize,
    pub tracks: Vec<TrackEntry>,
}

impl AlbumSummary {
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

/// Vue immuable d'un artiste ; `albums` est trié par année puis par nom.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistSummary {
    pub key: String,
    pub name: String,
    pub albums: Vec<AlbumSummary>,
}

impl ArtistSummary {
    pub fn track_count(&self) -> usize {
        self.albums.iter().map(|a| a.tracks.len()).sum()
    }
}

/// Ordre de lecture d'un album : disque, numéro de piste, puis nom de fichier en tri naturel.
pub fn track_order(a: &TrackEntry, b: &TrackEntry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let by_disc = a.disc_number.unwrap_or(0).cmp(&b.disc_number.unwrap_or(0));
    if by_disc != Ordering::Equal {
        return by_disc;
    }
    let by_track = a
        .track_number
        .unwrap_or(u32::MAX)
        .cmp(&b.track_number.unwrap_or(u32::MAX));
    if by_track != Ordering::Equal {
        return by_track;
    }
    natural_compare(&a.file_name, &b.file_name)
}

struct AlbumAcc {
    key: AlbumKey,
    name: String,
    name_score: i32,
    year: Option<i32>,
    discs: HashSet<u32>,
    tracks: Vec<TrackEntry>,
}

struct ArtistAcc {
    key: String,
    name: String,
    name_score: i32,
    albums: HashMap<String, AlbumAcc>,
}

/// Fusionne au fil de l'eau les lots de pistes émis par le scanner en artistes -> albums -> pistes.
///
/// Fusion multi-dossiers "sans écrasement" : deux sources qui contiennent le même artiste/album
/// alimentent la même entrée. Pas de fusion incrémentale par "dirty set" ici (contrairement à la
/// version Android) : un scan desktop reconstruit l'index en entier à chaque lancement, ce qui
/// reste rapide même à 50 000 pistes.
#[derive(Default)]
pub struct LibraryIndex {
    artists: HashMap<String, ArtistAcc>,
    track_count: usize,
}

impl LibraryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn track_count(&self) -> usize {
        self.track_count
    }

    pub fn artist_count(&self) -> usize {
        self.artists.len()
    }

    pub fn add_tracks(&mut self, tracks: Vec<TrackEntry>) {
        for track in tracks {
            self.add_track(track);
        }
    }

    fn add_track(&mut self, track: TrackEntry) {
        let folder = track.folder.clone();

        let artist = self
            .artists
            .entry(folder.artist_key.clone())
            .or_insert_with(|| ArtistAcc {
                key: folder.artist_key.clone(),
                name: String::new(),
                name_score: -1,
                albums: HashMap::new(),
            });
        // "Angèle" l'emporte sur "Angele", "Saez" sur "SAEZ" pour l'affichage.
        let artist_score = display_score(&folder.artist);
        if artist_score > artist.name_score {
            artist.name = folder.artist.clone();
            artist.name_score = artist_score;
        }

        let album = artist
            .albums
            .entry(folder.album_key.clone())
            .or_insert_with(|| AlbumAcc {
                key: AlbumKey {
                    artist_key: folder.artist_key.clone(),
                    album_key: folder.album_key.clone(),
                },
                name: String::new(),
                name_score: -1,
                year: None,
                discs: HashSet::new(),
                tracks: Vec::new(),
            });
        let album_score = display_score(&folder.album);
        if album_score > album.name_score {
            album.name = folder.album.clone();
            album.name_score = album_score;
        }
        if album.year.is_none() {
            album.year = folder.year;
        }
        if let Some(disc) = track.disc_number {
            album.discs.insert(disc);
        }

        album.tracks.push(track);
        self.track_count += 1;
    }

    /// Liste triée des artistes, prête pour l'interface (albums triés par année puis par nom,
    /// pistes triées par disque puis numéro de piste).
    pub fn snapshot(&self) -> Vec<ArtistSummary> {
        let mut out = Vec::with_capacity(self.artists.len());
        for artist in self.artists.values() {
            let mut albums: Vec<AlbumSummary> = artist
                .albums
                .values()
                .map(|acc| {
                    let mut tracks = acc.tracks.clone();
                    tracks.sort_by(track_order);
                    AlbumSummary {
                        key: acc.key.clone(),
                        name: acc.name.clone(),
                        year: acc.year,
                        disc_count: acc.discs.len(),
                        tracks,
                    }
                })
                .collect();
            albums.sort_by(|a, b| {
                let ya = a.year.unwrap_or(i32::MAX);
                let yb = b.year.unwrap_or(i32::MAX);
                if ya != yb {
                    ya.cmp(&yb)
                } else {
                    natural_compare(&a.name, &b.name)
                }
            });
            out.push(ArtistSummary {
                key: artist.key.clone(),
                name: artist.name.clone(),
                albums,
            });
        }
        out.sort_by(|a, b| natural_compare(&a.key, &b.key));
        out
    }
}

/// Index alphabétique A-Z (plus "#" pour chiffres, symboles et alphabets non latins).
pub const ALPHABET_OTHER: char = '#';

/// Lettre d'une clé d'artiste (voir [`crate::parse_path::keyify`] : minuscules, sans accents).
pub fn alphabet_letter_of(artist_key: &str) -> char {
    match artist_key.chars().next() {
        None => ALPHABET_OTHER,
        Some(c) => {
            let upper = c.to_ascii_uppercase();
            if ('A'..='Z').contains(&upper) {
                upper
            } else {
                ALPHABET_OTHER
            }
        }
    }
}

/// Nombre d'artistes par lettre (les lettres sans artiste sont absentes).
pub fn alphabet_counts(artist_keys: &[String]) -> HashMap<char, usize> {
    let mut counts = HashMap::new();
    for key in artist_keys {
        *counts.entry(alphabet_letter_of(key)).or_insert(0) += 1;
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(source: &str, path: &str, folder: ParsedFolder, disc: Option<u32>, track_no: Option<u32>) -> TrackEntry {
        TrackEntry {
            source_id: source.to_string(),
            path: path.to_string(),
            file_name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_string(),
            title: "Titre".to_string(),
            track_number: track_no,
            disc_number: disc,
            folder,
            dir_path: Vec::new(),
            size_bytes: 1234,
        }
    }

    fn folder(artist: &str, album: &str, year: Option<i32>) -> ParsedFolder {
        ParsedFolder {
            artist: artist.to_string(),
            artist_key: crate::parse_path::key_or_raw(artist),
            album: album.to_string(),
            album_key: crate::parse_path::key_or_raw(album),
            year,
            disc: None,
        }
    }

    #[test]
    fn merges_tracks_from_two_sources_into_one_artist() {
        let mut index = LibraryIndex::new();
        index.add_tracks(vec![
            track("src-a", "D:/A/Saez/Debbie/01.mp3", folder("Saez", "Debbie", None), None, Some(1)),
            track("src-b", "E:/B/Saez/Debbie/02.mp3", folder("Saez", "Debbie", None), None, Some(2)),
        ]);
        let snapshot = index.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].name, "Saez");
        assert_eq!(snapshot[0].albums.len(), 1);
        assert_eq!(snapshot[0].albums[0].tracks.len(), 2);
        assert_eq!(index.track_count(), 2);
    }

    #[test]
    fn accented_display_name_wins_over_plain_variant() {
        let mut index = LibraryIndex::new();
        index.add_tracks(vec![
            track("s", "1.mp3", folder("ANGELE", "Brol", None), None, Some(1)),
            track("s", "2.mp3", folder("Angèle", "Brol", None), None, Some(2)),
        ]);
        let snapshot = index.snapshot();
        assert_eq!(snapshot[0].name, "Angèle");
    }

    #[test]
    fn albums_are_ordered_by_year_then_name() {
        let mut index = LibraryIndex::new();
        index.add_tracks(vec![
            track("s", "1.mp3", folder("Radiohead", "In Rainbows", Some(2007)), None, Some(1)),
            track("s", "2.mp3", folder("Radiohead", "OK Computer", Some(1997)), None, Some(1)),
            track("s", "3.mp3", folder("Radiohead", "Sans année", None), None, Some(1)),
        ]);
        let albums = &index.snapshot()[0].albums;
        let names: Vec<&str> = albums.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["OK Computer", "In Rainbows", "Sans année"]);
    }

    #[test]
    fn tracks_are_ordered_by_disc_then_track_number() {
        let mut index = LibraryIndex::new();
        index.add_tracks(vec![
            track("s", "b.mp3", folder("A", "B", None), Some(1), Some(2)),
            track("s", "a.mp3", folder("A", "B", None), Some(1), Some(1)),
            track("s", "c.mp3", folder("A", "B", None), Some(2), Some(1)),
        ]);
        let tracks = &index.snapshot()[0].albums[0].tracks;
        let files: Vec<&str> = tracks.iter().map(|t| t.file_name.as_str()).collect();
        assert_eq!(files, vec!["a.mp3", "b.mp3", "c.mp3"]);
    }

    #[test]
    fn two_albums_with_the_same_name_from_different_artists_do_not_merge() {
        let mut index = LibraryIndex::new();
        index.add_tracks(vec![
            track("s", "1.mp3", folder("Artist A", "Greatest Hits", None), None, Some(1)),
            track("s", "2.mp3", folder("Artist B", "Greatest Hits", None), None, Some(1)),
        ]);
        assert_eq!(index.artist_count(), 2);
    }

    #[test]
    fn alphabet_letter_groups_digits_and_symbols_under_other() {
        assert_eq!(alphabet_letter_of("saez"), 'S');
        assert_eq!(alphabet_letter_of("1"), ALPHABET_OTHER);
        assert_eq!(alphabet_letter_of("!!!"), ALPHABET_OTHER);
        assert_eq!(alphabet_letter_of(""), ALPHABET_OTHER);
    }

    #[test]
    fn alphabet_counts_tally_by_first_letter() {
        let keys = vec!["saez".to_string(), "sinsemilia".to_string(), "angele".to_string()];
        let counts = alphabet_counts(&keys);
        assert_eq!(counts.get(&'S'), Some(&2));
        assert_eq!(counts.get(&'A'), Some(&1));
    }
}

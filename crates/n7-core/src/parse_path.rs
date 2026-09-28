//! ParsePath v6+ : déduit artiste / album / année / CD / titre à partir des seuls noms de
//! dossiers et de fichiers (jamais des tags audio, souvent mal renseignés).
//!
//! Port direct de `PathNormalizer.kt` (application Android n7Folder Player) : mêmes règles,
//! mêmes cas particuliers, mêmes tests. Toute divergence de comportement par rapport à
//! l'application Android serait un défaut de ce fichier.
//!
//! Deux moteurs d'expressions régulières sont utilisés :
//! - `regex` pour tous les motifs qui n'ont besoin ni d'antériorité (`lookbehind`) ni de
//!   postériorité (`lookahead`) : c'est le moteur le plus rapide et le plus robuste ;
//! - `fancy_regex` uniquement pour les 4 motifs qui utilisent `(?!...)` ou `(?<!...)`
//!   (année en tête de dossier, piste/disque préfixés, marqueur "feat/ft/avec"), fonctionnalité
//!   que `regex` n'implémente pas.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;

/// Nom d'artiste utilisé quand aucun dossier ne permet d'en déduire un.
pub const UNKNOWN_ARTIST: &str = "Artiste inconnu";
/// Nom d'album utilisé pour des pistes posées en vrac dans un dossier d'artiste.
pub const LOOSE_ALBUM: &str = "Sans album";

/// Résultat de l'analyse du chemin d'un dossier. Une seule instance est partagée par toutes les
/// pistes du même dossier (économie mémoire sur 50 000 fichiers).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedFolder {
    pub artist: String,
    pub artist_key: String,
    pub album: String,
    pub album_key: String,
    pub year: Option<i32>,
    pub disc: Option<u32>,
}

impl ParsedFolder {
    /// Identifiant d'album unique : deux albums homonymes d'artistes différents ne fusionnent pas.
    pub fn album_id(&self) -> String {
        format!("{}|{}", self.artist_key, self.album_key)
    }
}

/// Résultat de l'analyse d'un nom de fichier audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTrackName {
    pub track: Option<u32>,
    pub disc: Option<u32>,
    pub title: String,
}

struct AlbumParts {
    name: String,
    year: Option<i32>,
    disc: Option<u32>,
}

/// ParsePath v6+ : déduit artiste / album / année / CD / titre à partir des seuls noms de
/// dossiers.
///
/// `known_artist_keys` : clés (voir [`keyify`]) des dossiers artistes connus. Sert uniquement à
/// décider si "A & B" doit être regroupé sous "A" : on ne coupe que si un dossier "A" existe
/// réellement, ce qui protège "Simon & Garfunkel", "Hall & Oates", "Bigflo & Oli"…
#[derive(Debug, Clone, Default)]
pub struct PathNormalizer {
    known_artist_keys: HashSet<String>,
}

impl PathNormalizer {
    pub fn new(known_artist_keys: HashSet<String>) -> Self {
        Self { known_artist_keys }
    }

    /// `dir_segments` : noms des dossiers, de la racine jusqu'au dossier contenant le fichier
    /// (le nom du fichier n'en fait pas partie).
    pub fn parse_folder(&self, dir_segments: &[String]) -> ParsedFolder {
        let mut sig = significant_folders(dir_segments);

        // CD1 / Disc 2 : rattachés à l'album parent, jamais un niveau d'album parasite.
        let mut disc: Option<u32> = None;
        while sig.len() >= 2 {
            let allow_bare = sig.len() >= 3;
            let last = sig.last().expect("sig.len() >= 2").clone();
            match disc_number_of_folder(&last, allow_bare) {
                Some(d) => {
                    if disc.is_none() {
                        disc = Some(d);
                    }
                    sig.pop();
                }
                None => break,
            }
        }

        // Artiste/2004/Album : un dossier "année" seul n'est pas un album.
        let mut year: Option<i32> = None;
        if sig.len() >= 3 {
            if let Some(y) = year_only(&sig[1]) {
                year = Some(y);
                sig.remove(1);
            }
        }

        let mut artist_raw: Option<String> = None;
        let mut album_raw: Option<String> = None;
        if sig.len() == 1 {
            let lone = sig[0].clone();
            let lead = take_leading_year(&lone);
            let candidate = match &lead {
                Some((_, rest)) => rest.clone(),
                None => lone.clone(),
            };
            if let Some((a, b)) = split_artist_album(&candidate) {
                // "SAEZ - Debbie" ou "1997 - Radiohead - OK Computer"
                artist_raw = Some(a);
                album_raw = Some(b);
                if let Some((y, _)) = &lead {
                    year = Some(*y);
                }
            } else if lead.is_some() || take_trailing_year(&lone).is_some() {
                // "1970 - Black Sabbath" : c'est un album (année), jamais un artiste nommé "1970"
                album_raw = Some(lone);
            } else {
                // dossier artiste contenant des pistes en vrac
                artist_raw = Some(lone);
            }
        } else if sig.len() >= 2 {
            artist_raw = Some(sig[0].clone());
            album_raw = Some(sig[1].clone());
        }

        let artist = match &artist_raw {
            Some(raw) => self.clean_artist(raw),
            None => UNKNOWN_ARTIST.to_string(),
        };
        let artist_key = key_or_raw(&artist);

        let mut album_name = LOOSE_ALBUM.to_string();
        if let Some(raw) = &album_raw {
            let parts = self.clean_album(raw, &artist_key);
            album_name = parts.name;
            if parts.year.is_some() {
                year = parts.year;
            }
            if disc.is_none() {
                disc = parts.disc;
            }
        }
        let album_key = key_or_raw(&album_name);

        ParsedFolder {
            artist,
            artist_key,
            album: album_name,
            album_key,
            year,
            disc,
        }
    }

    /// `numbered_by_space` : vrai si tous les fichiers du dossier sont numérotés "01 Titre"
    /// (sans ponctuation). Évite de manger "99 Luftballons" ou "7 Rings" dans un dossier isolé.
    pub fn parse_track_name(
        &self,
        file_name: &str,
        artist_key: &str,
        numbered_by_space: bool,
    ) -> ParsedTrackName {
        let stripped = strip_extension(file_name);
        let mut base = collapse(stripped);
        if !base.contains(' ') && base.contains('_') {
            base = collapse(&base.replace('_', " "));
        }

        let mut track: Option<u32> = None;
        let mut disc: Option<u32> = None;
        let mut title = base.clone();

        let disc_track = fcaptures(re_disc_track(), &base);
        let disc_track_value = disc_track
            .as_ref()
            .and_then(|c| c.get(1))
            .and_then(|g| g.as_str().parse::<u32>().ok());
        let in_range = disc_track_value.map(|v| (1..=20).contains(&v)).unwrap_or(false);

        if in_range {
            let caps = disc_track.expect("in_range implique une capture");
            disc = disc_track_value;
            track = caps.get(2).and_then(|g| g.as_str().parse::<u32>().ok());
            title = caps.get(3).map(|g| g.as_str().to_string()).unwrap_or(base.clone());
        } else if let Some(caps) = fcaptures(re_track_punct(), &base) {
            track = caps.get(1).and_then(|g| g.as_str().parse::<u32>().ok());
            title = caps.get(2).map(|g| g.as_str().to_string()).unwrap_or(base.clone());
        } else if numbered_by_space {
            if let Some(caps) = re_track_space().captures(&base) {
                track = caps.get(1).and_then(|g| g.as_str().parse::<u32>().ok());
                title = caps.get(2).map(|g| g.as_str().to_string()).unwrap_or(base.clone());
            }
        }

        title = title.trim().to_string();
        if let Some(without_artist) = strip_artist_prefix(&title, artist_key) {
            title = without_artist;
        }
        if title.is_empty() {
            title = if !base.is_empty() { base } else { file_name.to_string() };
        }
        ParsedTrackName { track, disc, title }
    }

    // --------------------------------------------------------------------------------------
    // Étapes internes
    // --------------------------------------------------------------------------------------

    /// Nettoyage des invités : "Booba feat. Kaaris" -> "Booba" ; "A & B" -> "A" si "A" existe.
    fn clean_artist(&self, raw: &str) -> String {
        let mut name = collapse(raw);
        if let Some((_, rest)) = take_leading_year(&name) {
            name = rest;
        }
        name = cut_at_feat_marker(&name);
        name = self.cut_at_known_collab(&name);
        if name.trim().is_empty() {
            collapse(raw)
        } else {
            name
        }
    }

    fn cut_at_known_collab(&self, name: &str) -> String {
        if self.known_artist_keys.is_empty() {
            return name.to_string();
        }
        for m in re_collab_sep().find_iter(name) {
            let left = name[..m.start()].trim();
            if !left.is_empty() && self.known_artist_keys.contains(&key_or_raw(left)) {
                return left.to_string();
            }
        }
        name.to_string()
    }

    /// Élague artiste redondant, année, marqueur de CD et étiquettes de qualité du nom d'album.
    fn clean_album(&self, raw: &str, artist_key: &str) -> AlbumParts {
        let mut name = raw.to_string();
        let mut year: Option<i32> = None;
        let mut disc: Option<u32> = None;

        if let Some((y, rest)) = take_leading_year(&name) {
            year = Some(y);
            name = rest;
        }
        if let Some(rest) = strip_artist_prefix(&name, artist_key) {
            name = rest;
        }
        if year.is_none() {
            if let Some((y, rest)) = take_leading_year(&name) {
                year = Some(y);
                name = rest;
            }
        }

        let mut guard = 0;
        let mut changed = true;
        while changed && guard < 4 {
            changed = false;
            guard += 1;
            if let Some((y, rest)) = take_trailing_year(&name) {
                if year.is_none() {
                    year = Some(y);
                }
                name = rest;
                changed = true;
            }
            if let Some((d, rest)) = take_trailing_disc(&name) {
                if disc.is_none() {
                    disc = Some(d);
                }
                name = rest;
                changed = true;
            }
            if let Some(rest) = strip_quality_tag(&name) {
                name = rest;
                changed = true;
            }
        }
        if name.trim().is_empty() {
            name = raw.to_string();
        }
        AlbumParts {
            name: name.trim().to_string(),
            year,
            disc,
        }
    }
}

// ============================================================================================
// Fonctions libres (équivalent du "companion object" Kotlin)
// ============================================================================================

/// Retire les dossiers génériques de tête (1/, Musique/, FLAC/…) et les dossiers de format.
fn significant_folders(dir_segments: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut leading = true;
    for raw in dir_segments {
        let seg = clean_segment(raw);
        if seg.is_empty() {
            continue;
        }
        if leading && is_generic_container(&seg) {
            continue;
        }
        leading = false;
        if is_format_folder(&seg) {
            continue;
        }
        out.push(seg);
    }
    out
}

/// Clé d'identité : minuscules, sans accents latins, sans ponctuation, "&" = "and".
/// Angèle = Angele = ANGELE = "angele". Les autres écritures (kana, cyrillique, hangul…) sont
/// conservées telles quelles pour ne pas fusionner des noms différents.
pub fn keyify(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let mut sb = String::with_capacity(input.len() + 8);
    for ch in input.to_lowercase().chars() {
        match ch {
            'ß' => sb.push_str("ss"),
            'æ' => sb.push_str("ae"),
            'œ' => sb.push_str("oe"),
            'ø' => sb.push('o'),
            'đ' | 'ð' => sb.push('d'),
            'ł' => sb.push('l'),
            'þ' => sb.push_str("th"),
            'ı' => sb.push('i'),
            '&' => sb.push_str(" and "),
            other => sb.push(other),
        }
    }
    let decomposed: String = sb.nfkd().collect();
    let no_marks = re_latin_diacritics().replace_all(&decomposed, "");
    re_not_key_char().replace_all(&no_marks, "").into_owned()
}

/// Comme [`keyify`], mais un nom fait uniquement de symboles ("!!!") garde une clé non vide.
pub fn key_or_raw(name: &str) -> String {
    let k = keyify(name);
    if !k.is_empty() {
        k
    } else {
        name.trim().to_lowercase()
    }
}

const CONTAINER_KEYS: &[&str] = &[
    "musique",
    "musiques",
    "music",
    "musics",
    "audio",
    "audios",
    "son",
    "sons",
    "sound",
    "sounds",
    "song",
    "songs",
    "chanson",
    "chansons",
    "media",
    "medias",
    "library",
    "librairie",
    "bibliotheque",
    "collection",
    "collections",
    "download",
    "downloads",
    "telechargement",
    "telechargements",
    "sdcard",
    "storage",
    "emulated",
    "internal",
    "usb",
    "artists",
    "artistes",
    "interpretes",
    "albums",
];

const FORMAT_KEYS: &[&str] = &[
    "mp3", "mp3s", "flac", "flacs", "aac", "ogg", "opus", "wav", "m4a", "lossless", "lossy",
    "320", "320kbps", "256", "256kbps", "192", "128", "v0", "v2",
];

/// Dossier "conteneur" sans valeur d'artiste : Musique, FLAC, 1, 2, Downloads…
pub fn is_generic_container(name: &str) -> bool {
    let key = keyify(name);
    if key.is_empty() {
        return false;
    }
    if CONTAINER_KEYS.contains(&key.as_str()) || FORMAT_KEYS.contains(&key.as_str()) {
        return true;
    }
    key.len() <= 2 && key.chars().all(|c| c.is_ascii_digit())
}

pub fn is_format_folder(name: &str) -> bool {
    FORMAT_KEYS.contains(&keyify(name).as_str())
}

/// "CD1", "Disc 2", "Disque 1 - Bonus" -> numéro de disque.
/// `allow_bare` accepte aussi un simple "1" / "2" (à n'utiliser que sous un dossier d'album).
pub fn disc_number_of_folder(name: &str, allow_bare: bool) -> Option<u32> {
    let trimmed = name.trim();
    if let Some(caps) = re_disc_folder().captures(trimmed) {
        if let Some(g) = caps.get(1) {
            if let Ok(n) = g.as_str().parse::<u32>() {
                return Some(n);
            }
        }
    }
    if allow_bare
        && !trimmed.is_empty()
        && trimmed.len() <= 2
        && trimmed.chars().all(|c| c.is_ascii_digit())
    {
        if let Ok(n) = trimmed.parse::<u32>() {
            if (1..=30).contains(&n) {
                return Some(n);
            }
        }
    }
    None
}

/// Dossier qui hérite de la pochette de son parent (CD1, FLAC…).
pub fn is_disc_or_format_folder(name: &str) -> bool {
    disc_number_of_folder(name, true).is_some() || is_format_folder(name)
}

/// Clé d'artiste que produirait ce nom de dossier situé au premier niveau significatif.
pub fn artist_key_hint(folder_name: &str) -> String {
    let mut name = clean_segment(folder_name);
    if let Some((_, rest)) = take_leading_year(&name) {
        name = rest;
    }
    let artist_part = match split_artist_album(&name) {
        Some((a, _)) => a,
        None => name.clone(),
    };
    key_or_raw(&cut_at_feat_marker(&artist_part))
}

/// Vrai si tous les fichiers sont numérotés "NN Titre" avec des numéros distincts.
pub fn looks_space_numbered(file_names: &[String]) -> bool {
    if file_names.len() < 2 {
        return false;
    }
    let mut seen = HashSet::new();
    for name in file_names {
        match re_leading_number_space().captures(name.as_str()) {
            Some(caps) => {
                let n: i64 = match caps.get(1).and_then(|g| g.as_str().parse().ok()) {
                    Some(v) => v,
                    None => return false,
                };
                if !seen.insert(n) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// Meilleur nom d'affichage entre deux variantes : accents d'abord, puis casse mixte.
pub fn display_score(name: &str) -> i32 {
    let mut score = 0;
    for c in name.nfd() {
        if ('\u{0300}'..='\u{036f}').contains(&c) {
            score += 2;
        }
    }
    if name.chars().any(|c| c.is_lowercase()) {
        score += 1;
    }
    score
}

/// Tri naturel insensible à la casse : "Piste 2" < "Piste 10".
pub fn natural_compare(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        let (ca, cb) = (a[i], b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let mut ie = i;
            while ie < a.len() && a[ie].is_ascii_digit() {
                ie += 1;
            }
            let mut je = j;
            while je < b.len() && b[je].is_ascii_digit() {
                je += 1;
            }
            let na: String = a[i..ie].iter().collect::<String>();
            let nb: String = b[j..je].iter().collect::<String>();
            let na_trimmed = na.trim_start_matches('0');
            let nb_trimmed = nb.trim_start_matches('0');
            if na_trimmed.len() != nb_trimmed.len() {
                return na_trimmed.len().cmp(&nb_trimmed.len());
            }
            let c = na_trimmed.cmp(nb_trimmed);
            if c != Ordering::Equal {
                return c;
            }
            i = ie;
            j = je;
        } else {
            let c = ca.to_ascii_lowercase().cmp(&cb.to_ascii_lowercase());
            if c != Ordering::Equal {
                return c;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Retire l'extension seulement si elle ressemble à une vraie extension (1 à 5 lettres/chiffres).
pub fn strip_extension(file_name: &str) -> &str {
    match file_name.rfind('.') {
        Some(dot) if dot > 0 => {
            let ext = &file_name[dot + 1..];
            let len = ext.chars().count();
            if (1..=5).contains(&len) && ext.chars().all(|c| c.is_alphanumeric()) {
                &file_name[..dot]
            } else {
                file_name
            }
        }
        _ => file_name,
    }
}

// --------------------------------------------------------------------------------------------
// Helpers de texte (privés)
// --------------------------------------------------------------------------------------------

fn collapse(s: &str) -> String {
    re_spaces().replace_all(s.trim(), " ").into_owned()
}

/// "SAEZ_-_Debbie" (style scène, sans espaces) -> "SAEZ - Debbie".
fn clean_segment(raw: &str) -> String {
    let mut s = collapse(raw);
    if !s.contains(' ') && s.contains('_') {
        s = collapse(&s.replace('_', " "));
    }
    collapse(&s)
}

fn year_only(name: &str) -> Option<i32> {
    let trimmed = name.trim();
    let caps = re_year_only().captures(trimmed)?;
    caps.get(1)?.as_str().parse().ok()
}

/// "1970 - Nom", "1970_Nom", "[1970] Nom" -> (1970, "Nom"). "1989" ou "1989 (Deluxe)" : inchangé.
fn take_leading_year(name: &str) -> Option<(i32, String)> {
    if let Some(caps) = fcaptures(re_leading_year_dash(), name) {
        let year: i32 = caps.get(1)?.as_str().parse().ok()?;
        let rest = caps.get(2)?.as_str().trim().to_string();
        return Some((year, rest));
    }
    let caps = re_leading_year_bracket().captures(name)?;
    let year: i32 = caps.get(1)?.as_str().parse().ok()?;
    let rest = caps.get(2)?.as_str().trim().to_string();
    Some((year, rest))
}

/// "Nom (2008)", "Nom [2008]", "Nom - 2008".
fn take_trailing_year(name: &str) -> Option<(i32, String)> {
    let caps = re_trailing_year_bracket()
        .captures(name)
        .or_else(|| re_trailing_year_dash().captures(name))?;
    let year: i32 = caps.get(1)?.as_str().parse().ok()?;
    let whole = caps.get(0)?;
    let rest = name[..whole.start()].trim();
    if rest.is_empty() {
        return None;
    }
    Some((year, rest.to_string()))
}

/// "Nom (CD2)", "Nom - Disc 1", "Nom [Disque 2]".
fn take_trailing_disc(name: &str) -> Option<(u32, String)> {
    let caps = re_trailing_disc().captures(name)?;
    let disc: u32 = caps.get(1)?.as_str().parse().ok()?;
    let whole = caps.get(0)?;
    let rest = name[..whole.start()].trim();
    if rest.is_empty() {
        return None;
    }
    Some((disc, rest.to_string()))
}

/// "Nom [FLAC]", "Nom (320kbps)" -> "Nom".
fn strip_quality_tag(name: &str) -> Option<String> {
    let m = re_quality_tag().find(name)?;
    let rest = name[..m.start()].trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// "SAEZ - Debbie" -> ("SAEZ", "Debbie"). Refuse "1970 - X", "01 - X", "CD1 - X".
fn split_artist_album(name: &str) -> Option<(String, String)> {
    let m = re_split_artist_album().find(name)?;
    let left = name[..m.start()].trim();
    let right = name[m.end()..].trim();
    if left.is_empty() || right.is_empty() {
        return None;
    }
    if left.chars().all(|c| c.is_numeric()) {
        return None;
    }
    if disc_number_of_folder(left, false).is_some() {
        return None;
    }
    Some((left.to_string(), right.to_string()))
}

/// Si `name` commence par l'artiste suivi d'un tiret ("SAEZ - Debbie" sous le dossier "Saez"),
/// renvoie le reste ("Debbie"). Comparaison par [`keyify`] : accents et casse ignorés.
fn strip_artist_prefix(name: &str, artist_key: &str) -> Option<String> {
    if artist_key.is_empty() {
        return None;
    }
    for m in re_sep_any().find_iter(name) {
        let left = &name[..m.start()];
        if left.trim().is_empty() {
            continue;
        }
        let right = name[m.end()..].trim();
        if right.is_empty() {
            break;
        }
        if key_or_raw(left) == artist_key {
            return Some(right.to_string());
        }
    }
    None
}

/// "Booba feat. Kaaris", "Angèle (ft. Damso)", "Renaud avec Axelle Red" -> artiste principal.
fn cut_at_feat_marker(name: &str) -> String {
    let m = match re_feat_marker().find(name).ok().flatten() {
        Some(m) => m,
        None => return name.to_string(),
    };
    let head = name[..m.start()].trim_end_matches(|c: char| " ([,-–—&+/".contains(c));
    if head.trim().is_empty() {
        name.to_string()
    } else {
        head.to_string()
    }
}

// --------------------------------------------------------------------------------------------
// Expressions régulières compilées une seule fois (`OnceLock`)
// --------------------------------------------------------------------------------------------

/// Récupère les groupes capturés d'un motif `fancy_regex` sans propager son `Result` d'échec :
/// un échec de correspondance ou de moteur (rarissime, catastrophic backtracking) vaut "aucune
/// correspondance", jamais une panique.
fn fcaptures<'t>(re: &fancy_regex::Regex, text: &'t str) -> Option<fancy_regex::Captures<'t, str>> {
    re.captures(text).ok().flatten()
}

macro_rules! regex_fn {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static regex::Regex {
            static CELL: OnceLock<regex::Regex> = OnceLock::new();
            CELL.get_or_init(|| regex::Regex::new($pattern).expect("motif regex invalide"))
        }
    };
}

macro_rules! fancy_regex_fn {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static fancy_regex::Regex {
            static CELL: OnceLock<fancy_regex::Regex> = OnceLock::new();
            CELL.get_or_init(|| fancy_regex::Regex::new($pattern).expect("motif regex invalide"))
        }
    };
}

// -- Sans lookaround : moteur `regex` (rapide et éprouvé) -------------------------------------

regex_fn!(re_latin_diacritics, r"[\x{0300}-\x{036f}]+");
regex_fn!(re_not_key_char, r"[^\p{L}\p{N}\p{M}]+");
regex_fn!(re_spaces, r"\s+");
regex_fn!(
    re_leading_year_bracket,
    r"^[\[(]((?:19|20)\d{2})[\])]\s*[-–—_.]*\s*(\S.*)$"
);
regex_fn!(
    re_trailing_year_bracket,
    r"\s*[\[(]((?:19|20)\d{2})[\])]\s*$"
);
regex_fn!(re_trailing_year_dash, r"\s+[-–—]\s+((?:19|20)\d{2})\s*$");
regex_fn!(re_year_only, r"^((?:19|20)\d{2})$");
regex_fn!(
    re_disc_folder,
    r"(?i)^(?:cd|disc|disk|disque|disco|dvd)[\s._-]*0*(\d{1,2})(?:[\s._:(\[-].*)?$"
);
regex_fn!(
    re_trailing_disc,
    r"(?i)\s*[(\[\-_ ]\s*(?:cd|disc|disk|disque|disco)[\s._-]*0*(\d{1,2})\s*[)\]]?\s*$"
);
regex_fn!(
    re_quality_tag,
    r"(?i)\s*[\[(](?:flac|mp3|aac|ogg|opus|wav|alac|m4a|lossless|web|vinyl|24[\s-]?bit|\d{2,3}\s?kbps?)\b[^\])]*[\])]\s*$"
);
regex_fn!(re_split_artist_album, r"\s+[-–—]\s+");
regex_fn!(re_sep_any, r"\s*[-–—]+\s*");
regex_fn!(
    re_collab_sep,
    r"(?i)\s+(?:&|et|and|x|×|\+|/)\s+|\s*,\s+"
);
regex_fn!(re_track_space, r"^(\d{1,3})\s+(\S.*)$");
regex_fn!(re_leading_number_space, r"^(\d{1,3})\s+\S");

// -- Avec lookaround : moteur `fancy_regex` (seul à le supporter) -----------------------------

fancy_regex_fn!(
    re_leading_year_dash,
    r"^((?:19|20)\d{2})\s*[-–—_.]+\s*(?!\d{1,2}[-._]\d{1,2}\b)(\S.*)$"
);
fancy_regex_fn!(
    re_disc_track,
    r"^(\d{1,2})-(\d{2})(?!\d)[\s._)-]+(\S.*)$"
);
fancy_regex_fn!(
    re_track_punct,
    r"^(\d{1,3})(?!\d)\s*[-–—._):]+\s*(\S.*)$"
);
fancy_regex_fn!(
    re_feat_marker,
    r"(?i)(?<![\p{L}\p{N}])(?:feat(?:uring)?\.?|ft\.?|avec|w/)(?![\p{L}\p{N}])"
);

// ==============================================================================================
// Tests — port direct de PathNormalizerTest.kt : chaque cas correspond à un défaut réel corrigé
// dans ParsePath v6+. Toute régression ici est un vrai bug pour l'utilisateur.
// ==============================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn assert_folder(
        parser: &PathNormalizer,
        segments: &[&str],
        artist: &str,
        album: &str,
        year: Option<i32>,
        disc: Option<u32>,
    ) {
        let parsed = parser.parse_folder(&seg(segments));
        assert_eq!(parsed.artist, artist, "artiste de {segments:?}");
        assert_eq!(parsed.album, album, "album de {segments:?}");
        assert_eq!(parsed.year, year, "année de {segments:?}");
        assert_eq!(parsed.disc, disc, "disque de {segments:?}");
    }

    fn assert_track(
        title: &str,
        track: Option<u32>,
        disc: Option<u32>,
        file_name: &str,
        artist_key: &str,
        numbered_by_space: bool,
    ) {
        let normalizer = PathNormalizer::default();
        let parsed = normalizer.parse_track_name(file_name, artist_key, numbered_by_space);
        assert_eq!(parsed.title, title, "titre de {file_name}");
        assert_eq!(parsed.track, track, "piste de {file_name}");
        assert_eq!(parsed.disc, disc, "disque de {file_name}");
    }

    // --- keyify : Angèle = Angele = ANGELE -----------------------------------------------

    #[test]
    fn keyify_removes_accents_and_case() {
        assert_eq!(keyify("Angèle"), "angele");
        assert_eq!(keyify("ANGELE"), "angele");
        assert_eq!(keyify("angele"), "angele");
        assert_eq!(keyify("Björk"), "bjork");
        assert_eq!(keyify("Mötley Crüe"), "motleycrue");
        assert_eq!(keyify("Œuvres"), "oeuvres");
    }

    #[test]
    fn keyify_treats_ampersand_as_and() {
        assert_eq!(keyify("Simon and Garfunkel"), keyify("Simon & Garfunkel"));
    }

    #[test]
    fn key_or_raw_keeps_symbol_only_names() {
        assert_eq!(key_or_raw("!!!"), "!!!");
    }

    #[test]
    fn accented_and_plain_folders_share_the_same_artist_key() {
        let n = PathNormalizer::default();
        let a = n.parse_folder(&seg(&["Angèle", "Brol"]));
        let b = n.parse_folder(&seg(&["ANGELE", "Brol"]));
        assert_eq!(a.artist_key, b.artist_key);
        assert_eq!(a.album_id(), b.album_id());
    }

    // --- "Artiste - Album" (SAEZ - Debbie) ------------------------------------------------

    #[test]
    fn artist_dash_album_folder_is_split() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["Musique", "SAEZ - Debbie"], "SAEZ", "Debbie", None, None);
        assert_folder(&n, &["SAEZ_-_Debbie"], "SAEZ", "Debbie", None, None);
    }

    #[test]
    fn redundant_artist_prefix_in_album_folder_is_removed() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["Musique", "Saez", "SAEZ - Debbie"],
            "Saez",
            "Debbie",
            None,
            None,
        );
        assert_folder(&n, &["Saez", "Saez - Debbie"], "Saez", "Debbie", None, None);
    }

    #[test]
    fn split_and_nested_layouts_give_the_same_album_id() {
        let n = PathNormalizer::default();
        let flat = n.parse_folder(&seg(&["SAEZ - Debbie"]));
        let nested = n.parse_folder(&seg(&["Saez", "Debbie"]));
        assert_eq!(nested.album_id(), flat.album_id());
    }

    // --- Années (1970 - Black Sabbath) ------------------------------------------------------

    #[test]
    fn leading_year_folder_never_becomes_an_artist() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["Black Sabbath", "1970 - Black Sabbath"],
            "Black Sabbath",
            "Black Sabbath",
            Some(1970),
            None,
        );
        assert_folder(
            &n,
            &["1970 - Black Sabbath"],
            UNKNOWN_ARTIST,
            "Black Sabbath",
            Some(1970),
            None,
        );
        assert_folder(
            &n,
            &["Musique", "1970 - Black Sabbath"],
            UNKNOWN_ARTIST,
            "Black Sabbath",
            Some(1970),
            None,
        );
    }

    #[test]
    fn year_is_extracted_from_album_names() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["Black Sabbath", "Paranoid (1970)"],
            "Black Sabbath",
            "Paranoid",
            Some(1970),
            None,
        );
        assert_folder(
            &n,
            &["AC-DC", "Back in Black - 1980"],
            "AC-DC",
            "Back in Black",
            Some(1980),
            None,
        );
        assert_folder(
            &n,
            &["Jean-Jacques Goldman", "1984 - Positif"],
            "Jean-Jacques Goldman",
            "Positif",
            Some(1984),
            None,
        );
    }

    #[test]
    fn year_only_folder_between_artist_and_album_is_not_an_album() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["Radiohead", "2004", "Hail to the Thief"],
            "Radiohead",
            "Hail to the Thief",
            Some(2004),
            None,
        );
    }

    #[test]
    fn year_that_is_the_title_is_kept() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["Artist", "1989"], "Artist", "1989", None, None);
        assert_folder(
            &n,
            &["Taylor Swift", "1989 (Deluxe)"],
            "Taylor Swift",
            "1989 (Deluxe)",
            None,
            None,
        );
    }

    #[test]
    fn year_artist_album_in_one_folder() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["1997 - Radiohead - OK Computer"],
            "Radiohead",
            "OK Computer",
            Some(1997),
            None,
        );
    }

    // --- CD1 / Disc 2 ------------------------------------------------------------------------

    #[test]
    fn disc_folders_are_attached_to_the_parent_album() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["Pink Floyd", "The Wall", "CD1"], "Pink Floyd", "The Wall", None, Some(1));
        assert_folder(&n, &["Pink Floyd", "The Wall", "CD2"], "Pink Floyd", "The Wall", None, Some(2));
        assert_folder(
            &n,
            &["Pink Floyd", "The Wall", "Disque 1"],
            "Pink Floyd",
            "The Wall",
            None,
            Some(1),
        );
        assert_folder(
            &n,
            &["Pink Floyd", "The Wall", "CD 1 - Another Brick"],
            "Pink Floyd",
            "The Wall",
            None,
            Some(1),
        );
        assert_folder(&n, &["Pink Floyd", "The Wall", "1"], "Pink Floyd", "The Wall", None, Some(1));
        assert_folder(
            &n,
            &["Various Artists", "Now 50", "CD1"],
            "Various Artists",
            "Now 50",
            None,
            Some(1),
        );
    }

    #[test]
    fn disc_marker_inside_album_name_is_removed() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["Pink Floyd", "The Wall (CD2)"],
            "Pink Floyd",
            "The Wall",
            None,
            Some(2),
        );
        assert_folder(
            &n,
            &["Pink Floyd", "1979 - The Wall", "Disc 2"],
            "Pink Floyd",
            "The Wall",
            Some(1979),
            Some(2),
        );
    }

    // --- feat / avec / & ---------------------------------------------------------------------

    #[test]
    fn featured_guests_are_cut_from_the_artist() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["Booba feat. Kaaris", "Futur"], "Booba", "Futur", None, None);
        assert_folder(&n, &["Angèle (ft. Damso)", "Brol"], "Angèle", "Brol", None, None);
        assert_folder(
            &n,
            &["Renaud avec Axelle Red", "Best of"],
            "Renaud",
            "Best of",
            None,
            None,
        );
    }

    #[test]
    fn ampersand_duos_are_kept_unless_the_first_artist_exists() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["Simon & Garfunkel", "Bridge Over Troubled Water"],
            "Simon & Garfunkel",
            "Bridge Over Troubled Water",
            None,
            None,
        );
        assert_folder(
            &n,
            &["Hall & Oates", "Private Eyes"],
            "Hall & Oates",
            "Private Eyes",
            None,
            None,
        );

        let known: HashSet<String> = ["bigflo", "booba"].iter().map(|s| s.to_string()).collect();
        let with_known = PathNormalizer::new(known);
        assert_folder(
            &with_known,
            &["Bigflo & Oli", "La vraie vie"],
            "Bigflo",
            "La vraie vie",
            None,
            None,
        );
        assert_folder(
            &with_known,
            &["Booba x Kaaris", "Double Poney"],
            "Booba",
            "Double Poney",
            None,
            None,
        );
        assert_folder(
            &n,
            &["Bigflo & Oli", "La vraie vie"],
            "Bigflo & Oli",
            "La vraie vie",
            None,
            None,
        );
    }

    // --- Dossiers génériques et de format ------------------------------------------------------

    #[test]
    fn generic_leading_folders_are_skipped() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["1", "Booba feat. Kaaris"], "Booba", LOOSE_ALBUM, None, None);
        assert_folder(&n, &["Musique", "1", "Saez", "Debbie"], "Saez", "Debbie", None, None);
        assert_folder(
            &n,
            &["Musique", "FLAC", "Radiohead", "OK Computer"],
            "Radiohead",
            "OK Computer",
            None,
            None,
        );
        assert_folder(&n, &["Music", "Daft Punk", "Discovery"], "Daft Punk", "Discovery", None, None);
    }

    #[test]
    fn format_folders_and_quality_tags_are_removed() {
        let n = PathNormalizer::default();
        assert_folder(
            &n,
            &["FLAC", "Radiohead", "OK Computer [FLAC]"],
            "Radiohead",
            "OK Computer",
            None,
            None,
        );
        assert_folder(
            &n,
            &["Radiohead", "OK Computer", "FLAC"],
            "Radiohead",
            "OK Computer",
            None,
            None,
        );
        assert_folder(
            &n,
            &["Black Sabbath", "1970 - Paranoid [FLAC]"],
            "Black Sabbath",
            "Paranoid",
            Some(1970),
            None,
        );
    }

    #[test]
    fn empty_or_only_generic_paths_give_unknown_artist() {
        let n = PathNormalizer::default();
        assert_folder(&n, &[], UNKNOWN_ARTIST, LOOSE_ALBUM, None, None);
        assert_folder(&n, &["Musique"], UNKNOWN_ARTIST, LOOSE_ALBUM, None, None);
    }

    #[test]
    fn loose_tracks_in_an_artist_folder() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["Daft Punk"], "Daft Punk", LOOSE_ALBUM, None, None);
    }

    #[test]
    fn names_with_symbols_survive() {
        let n = PathNormalizer::default();
        assert_folder(&n, &["!!!", "Louden Up Now"], "!!!", "Louden Up Now", None, None);
        assert_folder(
            &n,
            &["Blink-182", "Enema of the State"],
            "Blink-182",
            "Enema of the State",
            None,
            None,
        );
    }

    // --- Noms de fichiers ----------------------------------------------------------------------

    #[test]
    fn track_numbers_with_punctuation() {
        assert_track("Titre", Some(1), None, "01 - Titre.mp3", "", false);
        assert_track("Titre", Some(1), None, "01. Titre.flac", "", false);
        assert_track("Hallelujah", Some(7), None, "07-Hallelujah.mp3", "", false);
    }

    #[test]
    fn disc_and_track_prefix() {
        assert_track("Titre", Some(5), Some(1), "1-05 Titre.mp3", "", false);
        assert_track("Titre", Some(11), Some(2), "2-11 - Titre.flac", "", false);
    }

    #[test]
    fn space_separated_numbers_only_when_the_whole_folder_is_numbered() {
        assert_track("01 Titre", None, None, "01 Titre.mp3", "", false);
        assert_track("Titre", Some(1), None, "01 Titre.mp3", "", true);
        assert_track("99 Luftballons", None, None, "99 Luftballons.mp3", "", false);
        assert_track("10 Years Gone", None, None, "10 Years Gone.mp3", "", false);
    }

    #[test]
    fn numbers_that_are_the_title_are_kept() {
        assert_track("1999", None, None, "1999.mp3", "", false);
        assert_track("01", None, None, "01.mp3", "", false);
    }

    #[test]
    fn redundant_artist_prefix_is_removed_from_titles() {
        assert_track("Debbie", None, None, "SAEZ - Debbie.mp3", "saez", false);
        assert_track("Titre", None, None, "Saez_-_Titre.mp3", "saez", false);
        assert_track("Titre", Some(3), None, "03 - Saez - Titre.mp3", "saez", false);
        assert_track(
            "Bohemian Rhapsody - Queen",
            None,
            None,
            "Bohemian Rhapsody - Queen.mp3",
            "queen",
            false,
        );
    }

    #[test]
    fn plain_titles_are_untouched() {
        assert_track("Titre sans numéro", None, None, "Titre sans numéro.opus", "", false);
        assert_track(
            "Mon.Titre.Avec.Points",
            None,
            None,
            "Mon.Titre.Avec.Points.mp3",
            "",
            false,
        );
        assert_track("12 Titre Underscore", None, None, "12_Titre_Underscore.mp3", "", false);
    }

    #[test]
    fn looks_space_numbered_requires_distinct_numbers_on_every_file() {
        assert!(looks_space_numbered(&seg(&["01 A.mp3", "02 B.mp3", "03 C.mp3"])));
        assert!(!looks_space_numbered(&seg(&["01 A.mp3", "B.mp3"])));
        assert!(!looks_space_numbered(&seg(&["01 A.mp3", "01 B.mp3"])));
        assert!(!looks_space_numbered(&seg(&["99 Luftballons.mp3"])));
    }

    // --- Tri et affichage ------------------------------------------------------------------------

    #[test]
    fn natural_compare_sorts_numbers_by_value() {
        use std::cmp::Ordering;
        assert_eq!(natural_compare("Piste 2", "Piste 10"), Ordering::Less);
        assert_eq!(natural_compare("a10", "a9"), Ordering::Greater);
        assert_eq!(natural_compare("ABC", "abc"), Ordering::Equal);
    }

    #[test]
    fn display_score_prefers_accented_and_mixed_case_variants() {
        assert!(display_score("Angèle") > display_score("Angele"));
        assert!(display_score("Saez") > display_score("SAEZ"));
    }

    #[test]
    fn strip_extension_only_removes_real_extensions() {
        assert_eq!(strip_extension("a.mp3"), "a");
        assert_eq!(strip_extension("Mr. Brightside"), "Mr. Brightside");
        assert_eq!(strip_extension(".hidden"), ".hidden");
    }
}

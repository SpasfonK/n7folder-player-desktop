//! Pochettes : détection du fichier local dans le dossier d'un album, et génération de miniatures
//! mises en cache sur disque.
//!
//! Ordre de priorité prévu par la mission : fichier local du dossier > tags audio embarqués > API
//! externe (TheAudioDB). Cette itération couvre le premier niveau (fichier local) et le cache de
//! miniatures ; les tags embarqués et l'appel réseau sont prévus pour un prochain passage — une
//! pochette manquante aujourd'hui n'empêche jamais la lecture, elle reste simplement absente.

use std::path::{Path, PathBuf};

/// Noms de fichiers considérés comme pochette de dossier, par ordre de préférence. Comparaison
/// insensible à la casse : `Cover.JPG`, `COVER.JPG` et `cover.jpg` sont équivalents.
const CANDIDATE_NAMES: &[&str] = &[
    "cover.jpg",
    "cover.jpeg",
    "cover.png",
    "folder.jpg",
    "folder.jpeg",
    "folder.png",
    "artwork.jpg",
    "artwork.jpeg",
    "artwork.png",
    "front.jpg",
    "front.jpeg",
    "front.png",
];

/// Cherche un fichier de pochette directement dans `dir` (jamais dans ses sous-dossiers : un
/// CD1/CD2 a sa propre pochette, ou en hérite explicitement — logique laissée à l'appelant).
pub fn find_local_cover(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(usize, PathBuf)> = None;
    for entry in entries.flatten() {
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let Some(rank) = CANDIDATE_NAMES.iter().position(|c| *c == name.as_str()) else {
            continue;
        };
        if best.as_ref().map(|(r, _)| rank < *r).unwrap_or(true) {
            best = Some((rank, entry.path()));
        }
    }
    best.map(|(_, path)| path)
}

/// Chemin de la miniature en cache pour une clé source donnée (chemin du fichier original,
/// mis en minuscules). Clé hachée plutôt que copiée telle quelle : un chemin Windows complet
/// contient des caractères (`\`, `:`) interdits dans un nom de fichier.
pub fn thumbnail_cache_path(cache_dir: &Path, source_key: &str) -> PathBuf {
    let hash = fnv1a(source_key.to_lowercase().as_bytes());
    cache_dir.join("covers").join(format!("{hash:016x}.jpg"))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Échec de génération de miniature : jamais fatal pour l'appelant. Une pochette manquante ou
/// abîmée ne doit jamais empêcher la lecture de la piste, seulement l'affichage de son image.
#[derive(Debug)]
pub enum ThumbnailError {
    Io(std::io::Error),
    Image(image::ImageError),
}

impl std::fmt::Display for ThumbnailError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThumbnailError::Io(e) => write!(f, "erreur d'entrée/sortie : {e}"),
            ThumbnailError::Image(e) => write!(f, "image illisible ou impossible à encoder : {e}"),
        }
    }
}

impl std::error::Error for ThumbnailError {}

/// Génère (si besoin) une miniature JPEG d'au plus `max_side` pixels de côté et renvoie son
/// chemin sur disque. Ne régénère rien si une miniature déjà en cache est au moins aussi récente
/// que l'image source (évite de refaire le travail à chaque scan).
pub fn ensure_thumbnail(
    source_image_path: &Path,
    cache_dir: &Path,
    source_key: &str,
    max_side: u32,
) -> Result<PathBuf, ThumbnailError> {
    let thumb_path = thumbnail_cache_path(cache_dir, source_key);

    if is_cache_fresh(&thumb_path, source_image_path) {
        return Ok(thumb_path);
    }

    let bytes = std::fs::read(source_image_path).map_err(ThumbnailError::Io)?;
    let decoded = image::load_from_memory(&bytes).map_err(ThumbnailError::Image)?;
    let thumb = decoded.thumbnail(max_side, max_side).to_rgb8();

    if let Some(parent) = thumb_path.parent() {
        std::fs::create_dir_all(parent).map_err(ThumbnailError::Io)?;
    }
    // Écriture atomique : un lecteur qui affiche la miniature pendant la régénération ne voit
    // jamais un fichier tronqué.
    let tmp_path = thumb_path.with_extension("jpg.tmp");
    thumb
        .save_with_format(&tmp_path, image::ImageFormat::Jpeg)
        .map_err(ThumbnailError::Image)?;
    std::fs::rename(&tmp_path, &thumb_path).map_err(ThumbnailError::Io)?;
    Ok(thumb_path)
}

fn is_cache_fresh(thumb_path: &Path, source_path: &Path) -> bool {
    let (Ok(thumb_meta), Ok(source_meta)) = (std::fs::metadata(thumb_path), std::fs::metadata(source_path))
    else {
        return false;
    };
    match (thumb_meta.modified(), source_meta.modified()) {
        (Ok(t), Ok(s)) => t >= s,
        _ => true, // horodatage indisponible sur cette plateforme : mieux vaut garder le cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("n7covers-{tag}-{}-{nanos}", std::process::id()))
    }

    fn tiny_png(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut img = image::RgbImage::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                img.put_pixel(x, y, image::Rgb([(x * 30) as u8, (y * 30) as u8, 128]));
            }
        }
        img.save_with_format(path, image::ImageFormat::Png).unwrap();
    }

    #[test]
    fn finds_cover_case_insensitively() {
        let dir = temp_dir("cover-case");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cover.JPG"), b"donnees").unwrap();
        std::fs::write(dir.join("autre-fichier.txt"), b"donnees").unwrap();

        let found = find_local_cover(&dir).expect("pochette attendue");
        assert_eq!(found.file_name().unwrap().to_str().unwrap(), "Cover.JPG");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prefers_cover_over_folder_when_both_present() {
        let dir = temp_dir("cover-priority");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("folder.jpg"), b"donnees").unwrap();
        std::fs::write(dir.join("cover.jpg"), b"donnees").unwrap();

        let found = find_local_cover(&dir).unwrap();
        assert_eq!(found.file_name().unwrap().to_str().unwrap(), "cover.jpg");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_candidate_gives_none_without_erroring() {
        let dir = temp_dir("cover-none");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("01 - Titre.mp3"), b"donnees").unwrap();
        assert_eq!(find_local_cover(&dir), None);
        std::fs::remove_dir_all(&dir).ok();

        let missing = temp_dir("cover-missing-dir");
        assert_eq!(find_local_cover(&missing), None);
    }

    #[test]
    fn thumbnail_cache_path_is_deterministic_and_distinct() {
        let cache = Path::new("D:/cache");
        let a = thumbnail_cache_path(cache, "D:/Musique/Saez/Debbie/cover.jpg");
        let b = thumbnail_cache_path(cache, "D:/Musique/Saez/Debbie/cover.jpg");
        let c = thumbnail_cache_path(cache, "D:/Musique/Angele/Brol/cover.jpg");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with(cache.join("covers")));
    }

    #[test]
    fn generates_and_reuses_a_thumbnail() {
        let dir = temp_dir("thumb-ok");
        let source = dir.join("source.png");
        tiny_png(&source);
        let cache_dir = dir.join("cache");

        let path1 = ensure_thumbnail(&source, &cache_dir, "clef-1", 4).expect("miniature générée");
        assert!(path1.exists());
        let decoded = image::open(&path1).expect("miniature lisible");
        assert!(decoded.width() <= 4 && decoded.height() <= 4);

        // Deuxième appel : le cache est déjà à jour, doit renvoyer le même chemin sans erreur.
        let path2 = ensure_thumbnail(&source, &cache_dir, "clef-1", 4).expect("cache réutilisé");
        assert_eq!(path1, path2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_source_image_fails_without_panicking() {
        let dir = temp_dir("thumb-corrupt");
        let source = dir.join("source.png");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&source, b"ceci n'est pas une image").unwrap();
        let cache_dir = dir.join("cache");

        let result = ensure_thumbnail(&source, &cache_dir, "clef-corrompue", 64);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}

# n7Folder Player (Windows 10 / 11)

Lecteur de musique **par dossiers** (jamais par tags), avec la surface d'artistes zoomable de n7player.
Rust + Tauri v2, interface TypeScript. Voir `docs/ARCHITECTURE.md`.

## État : itération 3 sur 5 — moteur audio & pochettes (première partie)

Itération 1 (socle) : fenêtre Mica/acrylique, dossiers sources cumulatifs (disque local, USB,
`\\NAS\Musique`), relink à identifiant stable, disponibilité de chaque source en temps réel, réglages
persistés de façon atomique dans `%APPDATA%\n7FolderPlayer\settings.json`, manifeste Windows (chemins
longs, UTF-8, DPI), compteur de fps.

Itération 2 (nouveau) : `crates/n7-core/src/parse_path.rs` porte `ParsePath v6+` (déaccentuation,
« Artiste - Album », années, `CD1`/`Disc 2`, `feat/avec/&`, dossiers génériques et de format) avec les
mêmes cas de test que l'application Android d'origine ; `scanner.rs` scanne toutes les sources en
parallèle (un thread par source, parcours itératif, jamais de blocage de l'interface, jamais d'arrêt sur
un dossier illisible) ; `library.rs` fusionne les pistes en artistes/albums sans écraser une source par
une autre ; `cache.rs` enregistre l'index dans `%APPDATA%\n7FolderPlayer\library.json` (écriture atomique).
Un bouton **Scanner la bibliothèque** dans l'interface déclenche tout cela et affiche le nombre de pistes,
d'artistes, les sources injoignables et un compteur par artiste.

Itération 3 (nouveau) : `src-tauri/src/audio.rs` — moteur audio `rodio` sur un thread dédié (lecture, pause,
arrêt, volume, positionnement, position de lecture émise 4 fois par seconde, fin de piste détectée) ;
`crates/n7-core/src/queue.rs` — file de lecture pure (aléatoire qui garde la piste courante, répétition
off/tout/piste, précédent/suivant) testée sans carte son ; `covers.rs` — détection de `cover.jpg` /
`folder.jpg` / `artwork.png` (casse ignorée) et cache de miniatures JPEG. Une carte **Lecteur** permet de
tester : choisir un artiste scanné, double-cliquer une piste, contrôler lecture, volume, aléatoire, répétition.

Limites connues de cette étape : formats lus = MP3, FLAC, WAV, OGG Vorbis (AAC/M4A/Opus indexés mais pas
encore lus) ; lecture sans blanc entre pistes non garantie (piste suivante chargée à la fin de la précédente) ;
pochettes : seul le fichier local est géré (tags embarqués et TheAudioDB restent à faire) et les miniatures ne
sont pas encore affichées (surface n7player = itération 4).

## Compiler en local

Prérequis : Rust stable (MSVC), Node 20+, « Build Tools for Visual Studio » (charge C++), WebView2.

```powershell
npm install
npm run tauri dev             # développement
npx tauri build               # release
cargo test -p n7-core         # tests du coeur (parseur + scanner + cache)
cargo clippy --workspace --all-targets -- -D warnings
```

Livrables : `target\release\n7-folder-player.exe` (portable) et `target\release\bundle\nsis\*.exe` (installateur).

## Compiler via GitHub Actions

`.github/workflows/build-windows.yml` : job `quality` (typage, tests, clippy `-D warnings`), job `build`
(artefact `n7folder-player-windows-x64`), job `release` (sur un tag `v*`). Lancement manuel possible
(*Actions → build-windows → Run workflow*).

## Limites de vérification connues

Ce code a été écrit dans un environnement **sans Rust, sans réseau et sans Windows** : ni `cargo` ni `tauri build`
n'ont pu y être exécutés. Vérifié réellement : syntaxe TypeScript, validité du YAML/JSON/TOML, cohérence des
chemins. Le premier passage du workflow est la vraie validation ; en cas d'échec, coller le journal suffit.
Les fichiers `package-lock.json` et `Cargo.lock` seront à committer après ce premier passage.

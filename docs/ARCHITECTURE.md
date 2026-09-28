# Architecture — n7Folder Player (Windows 10/11)

## Stack retenue : Rust + Tauri v2, interface TypeScript

| Couche | Choix | Pourquoi |
| --- | --- | --- |
| Coquille | Tauri v2 (WebView2) | Exécutable de quelques Mo, WebView2 déjà présent sur Windows 11 et sur la plupart des Windows 10, Mica/acrylique via `window-vibrancy`. |
| Coeur | Rust (crate `n7-core`) | Scan disque, parseur de chemins, index et audio dans des threads natifs : aucun gel de l'interface, mémoire maîtrisée avec 50 000 pistes. |
| Interface | Vite + TypeScript, sans framework | Surface 2D zoomable dessinée en WebGL/Canvas (itération 4) ; un framework de composants n'apporte rien à un canevas. |
| Build | GitHub Actions `windows-latest` | Rust, Node et NSIS sont gratuits ; aucun outil propriétaire payant. |

Options écartées : **.NET/WinUI 3** (excellent Fluent, mais canevas 2D infini et zoom sémantique à écrire en Composition/Direct2D, packaging MSIX plus lourd) ; **Qt 6** (licence LGPL à respecter, binaire plus gros) ; **Electron** (accepté par le cahier des charges, mais 100+ Mo et une mémoire de base bien supérieure pour le même résultat).

## Découpage

```
crates/n7-core      Logique pure, sans Tauri : testable partout, très vite
  paths.rs          %APPDATA%\n7FolderPlayer
  settings.rs       Dossiers sources : ajout cumulatif, relink à identifiant stable, JSON atomique
  parse_path.rs     ParsePath v6+ : artiste/album/année/CD à partir des noms de dossiers (regex + fancy-regex)
  scanner.rs        Scan multithreadé (un thread par source), parcours itératif, jamais de blocage
  library.rs        Fusion pistes -> artistes -> albums, tri, index alphabétique
  queue.rs          File de lecture : aléatoire, répétition, suivant/précédent (sans audio, testable en CI)
  covers.rs         Pochette locale (cover/folder/artwork) + miniatures JPEG en cache
  cache.rs          library.json (écriture atomique, comme settings.rs)
src-tauri           Fine couche de commandes (async + spawn_blocking pour tout accès disque)
  src/audio.rs           Moteur audio rodio sur un thread dédié ; le reste lui parle par canal mpsc
  windows/app.manifest   Chemins longs, UTF-8, DPI par moniteur, Windows 10/11
src/                Interface (TypeScript)
```

`parse_path.rs` utilise deux moteurs de regex : `regex` (rapide) pour tout motif sans antériorité/postériorité, et `fancy_regex` uniquement pour les 4 motifs qui en ont besoin (année en tête de dossier, piste/disque préfixés, marqueur `feat/ft/avec`) — `regex` ne supporte pas `(?!...)`/`(?<!...)`.

Règle : tout accès disque ou réseau part sur un thread d'arrière-plan (`spawn_blocking`). Un partage UNC éteint ne peut donc bloquer que sa propre vérification (`probe_source`, un appel par source).

## Windows

* **Chemins > 260 caractères** : manifeste `longPathAware` ; la bibliothèque standard de Rust préfixe elle-même les chemins absolus longs (`\\?\`). Pour les API Win32 hors Rust, la clé `LongPathsEnabled` reste à activer côté système.
* **Unicode** : page de code active UTF-8 dans le manifeste ; les chemins restent des `OsString` côté Rust.
* **Mica / acrylique** : Mica sur Windows 11, repli acrylique sinon. La fenêtre est transparente et le fond de la page aussi.
* **WebView2** : l'installateur NSIS télécharge le runtime s'il manque. L'exécutable portable suppose qu'il est déjà installé.

## Plan des itérations (ce qui sera ajouté au coeur)

2. ✅ `parse_path` (port de `PathNormalizer.kt`, mêmes cas de test : Saez, Black Sabbath, CD1/CD2, accents, années, feat/avec/&), scanner multithreadé multi-dossiers (`std::fs`, parcours itératif), fusion en artistes/albums, cache `library.json`.
3. 🟡 Audio natif `rodio` (thread dédié, commandes par canal) + file de lecture pure + pochettes fichier local et cache de miniatures : fait. Reste : tags embarqués (`lofty`), TheAudioDB (clé de test `2`), AAC/M4A/Opus (`symphonia`), lecture sans blanc, égaliseur 10 bandes et FFT (avec le visualiseur, itération 4).
4. Surface zoomable (PixiJS/WebGL, texte en atlas, culling et niveaux de détail), mini-lecteur, A–Z, visualiseur.
5. Mesures d'échelle (50 000 fichiers), optimisation mémoire, notice d'installation.

Point à trancher en itération 3 : `symphonia` ne couvre pas Opus ni WMA ; à évaluer selon le contenu réel des bibliothèques.

Optimisation notée pour l'itération 5 : `scan_library` (`src-tauri/src/main.rs`) clone actuellement la
liste complète des pistes une fois (cache disque + index en mémoire) — à fusionner en un seul passage
une fois la bibliothèque testée à grande échelle.

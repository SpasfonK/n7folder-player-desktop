import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import "./style.css";

interface AppInfo {
  name: string;
  version: string;
  dataDir: string;
  os: string;
  arch: string;
}

interface MusicSource {
  id: string;
  path: string;
  label: string;
  rootIsArtist: boolean;
}

interface AddResult {
  outcome: "added" | "duplicate" | "nested";
  detail: string | null;
  sources: MusicSource[];
}

interface ScanStats {
  files: number;
  dirs: number;
  skipped: number;
}

interface SourceScanOutcome {
  sourceId: string;
  label: string;
  unavailable: boolean;
  stats: ScanStats;
}

interface ArtistLite {
  key: string;
  name: string;
  albumCount: number;
  trackCount: number;
}

interface ScanReport {
  totalFiles: number;
  elapsedMs: number;
  artistCount: number;
  sources: SourceScanOutcome[];
  artists: ArtistLite[];
}

interface TrackLite {
  path: string;
  title: string;
  artist: string;
  album: string;
  trackNumber: number | null;
}

interface PlayerTick {
  path: string | null;
  positionMs: number;
  playing: boolean;
  ended: boolean;
}

type RepeatMode = "off" | "all" | "one";

type Availability = "checking" | "online" | "offline";
type MessageKind = "ok" | "warn" | "error";

const state = {
  info: null as AppInfo | null,
  sources: [] as MusicSource[],
  availability: new Map<string, Availability>(),
  message: null as { kind: MessageKind; text: string } | null,
  busy: false,
  scanning: false,
  scanReport: null as ScanReport | null,
  playerArtistKey: "",
  playerTracks: [] as TrackLite[],
  playerPath: null as string | null,
  playerPositionMs: 0,
  playerPlaying: false,
  playerVolume: 0.8,
  playerShuffle: false,
  playerRepeat: "off" as RepeatMode,
};

const root = document.getElementById("app");
if (!root) {
  throw new Error("conteneur #app introuvable");
}

// Compteur d'images/seconde : outil de la boucle Gauntlet (fluidité du canevas aux itérations suivantes).
const fpsNode = el("span", "fps", "… fps");

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return String(error);
}

function say(kind: MessageKind, text: string): void {
  state.message = { kind, text };
  render();
}

async function pickFolder(title: string): Promise<string | null> {
  const selected = await open({ directory: true, multiple: false, title });
  return typeof selected === "string" ? selected : null;
}

function probe(id: string): void {
  state.availability.set(id, "checking");
  void invoke<boolean>("probe_source", { id })
    .then((ok): Availability => (ok ? "online" : "offline"))
    .catch((): Availability => "offline")
    .then((status) => {
      if (state.sources.some((s) => s.id === id)) {
        state.availability.set(id, status);
        render();
      }
    });
}

function applySources(sources: MusicSource[], reprobe: string[] = []): void {
  state.sources = sources;
  const alive = new Set(sources.map((s) => s.id));
  for (const id of [...state.availability.keys()]) {
    if (!alive.has(id)) state.availability.delete(id);
  }
  for (const source of sources) {
    if (!state.availability.has(source.id) || reprobe.includes(source.id)) probe(source.id);
  }
  render();
}

async function addFolder(): Promise<void> {
  if (state.busy) return;
  state.busy = true;
  render();
  try {
    const path = await pickFolder("Ajouter un dossier de musique");
    if (path === null) return;
    const result = await invoke<AddResult>("add_source", { path });
    applySources(result.sources);
    if (result.outcome === "added") {
      say("ok", `Dossier ajouté : ${path}`);
    } else if (result.outcome === "duplicate") {
      say("warn", "Ce dossier est déjà dans la bibliothèque.");
    } else {
      say("warn", `Ce dossier est déjà couvert par « ${result.detail ?? "une source existante"} ».`);
    }
  } catch (error) {
    say("error", errorText(error));
  } finally {
    state.busy = false;
    render();
  }
}

async function relinkFolder(source: MusicSource): Promise<void> {
  if (state.busy) return;
  state.busy = true;
  render();
  try {
    const path = await pickFolder(`Nouvel emplacement de « ${source.label} »`);
    if (path === null) return;
    const sources = await invoke<MusicSource[]>("relink_source", { id: source.id, newPath: path });
    applySources(sources, [source.id]);
    say("ok", `« ${source.label} » relié à ${path}`);
  } catch (error) {
    say("error", errorText(error));
  } finally {
    state.busy = false;
    render();
  }
}

async function removeFolder(source: MusicSource): Promise<void> {
  try {
    const sources = await invoke<MusicSource[]>("remove_source", { id: source.id });
    applySources(sources);
    say("ok", `« ${source.label} » retiré de la bibliothèque (vos fichiers ne sont pas touchés).`);
  } catch (error) {
    say("error", errorText(error));
  }
}

function formatDuration(ms: number): string {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

async function scanLibrary(): Promise<void> {
  if (state.scanning) return;
  state.scanning = true;
  render();
  try {
    const report = await invoke<ScanReport>("scan_library");
    state.scanReport = report;
    const unavailable = report.sources.filter((s) => s.unavailable).length;
    const base = `Scan terminé : ${report.totalFiles} piste(s), ${report.artistCount} artiste(s).`;
    say(
      unavailable > 0 ? "warn" : "ok",
      unavailable > 0 ? `${base} ${unavailable} source(s) injoignable(s).` : base,
    );
  } catch (error) {
    say("error", errorText(error));
  } finally {
    state.scanning = false;
    render();
  }
}

function formatClock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/** Met à jour l'affichage de la position sans reconstruire la page (sinon les listes et curseurs perdraient le focus 4 fois par seconde). */
function paintPlayerClock(): void {
  const node = document.getElementById("player-clock");
  if (node) node.textContent = formatClock(state.playerPositionMs);
  const now = document.getElementById("player-now");
  if (now) {
    const track = state.playerTracks.find((t) => t.path === state.playerPath);
    now.textContent = track ? `${track.title} — ${track.artist} (${track.album})` : "Aucune piste en lecture";
  }
  const toggle = document.getElementById("player-toggle");
  if (toggle) toggle.textContent = state.playerPlaying ? "Pause" : "Lecture";
}

async function loadArtistTracks(artistKey: string): Promise<void> {
  state.playerArtistKey = artistKey;
  state.playerTracks = [];
  render();
  if (artistKey === "") return;
  try {
    state.playerTracks = await invoke<TrackLite[]>("list_tracks_for_artist", { artistKey });
  } catch (error) {
    say("error", errorText(error));
  }
  render();
}

async function playFromTrack(path: string): Promise<void> {
  try {
    await invoke("player_set_queue", { trackIds: state.playerTracks.map((t) => t.path) });
    await invoke("player_jump_to", { trackId: path });
  } catch (error) {
    say("error", errorText(error));
  }
}

async function togglePlayback(): Promise<void> {
  try {
    await invoke(state.playerPlaying ? "player_pause" : "player_play");
  } catch (error) {
    say("error", errorText(error));
  }
}

async function skip(direction: "player_next" | "player_previous"): Promise<void> {
  try {
    await invoke(direction);
  } catch (error) {
    say("error", errorText(error));
  }
}

async function setShuffle(on: boolean): Promise<void> {
  state.playerShuffle = on;
  try {
    await invoke("player_set_shuffle", { on });
  } catch (error) {
    say("error", errorText(error));
  }
}

async function cycleRepeat(): Promise<void> {
  const order: RepeatMode[] = ["off", "all", "one"];
  const next = order[(order.indexOf(state.playerRepeat) + 1) % order.length] ?? "off";
  state.playerRepeat = next;
  try {
    await invoke("player_set_repeat", { mode: next });
  } catch (error) {
    say("error", errorText(error));
  }
  render();
}

function repeatLabel(mode: RepeatMode): string {
  if (mode === "all") return "Répéter : tout";
  if (mode === "one") return "Répéter : piste";
  return "Répéter : non";
}

function playerCard(): HTMLElement {
  const card = el("section", "card");
  card.append(el("h2", undefined, "Lecteur (test du moteur audio)"));
  card.append(
    el(
      "p",
      "hint",
      "Choisissez un artiste scanné puis une piste. Formats lus pour l'instant : MP3, FLAC, WAV, OGG Vorbis.",
    ),
  );

  const artistSelect = el("select");
  artistSelect.append(new Option("— choisir un artiste —", ""));
  for (const artist of state.scanReport?.artists ?? []) {
    artistSelect.append(new Option(`${artist.name} (${artist.trackCount})`, artist.key));
  }
  artistSelect.value = state.playerArtistKey;
  artistSelect.addEventListener("change", () => void loadArtistTracks(artistSelect.value));
  card.append(artistSelect);

  if (state.playerTracks.length > 0) {
    const list = el("ul", "track-list");
    for (const track of state.playerTracks) {
      const item = el("li", track.path === state.playerPath ? "track-row current" : "track-row");
      const number = track.trackNumber === null ? "" : `${track.trackNumber}. `;
      item.append(el("span", "name", `${number}${track.title}`));
      item.append(el("span", "count", track.album));
      item.addEventListener("dblclick", () => void playFromTrack(track.path));
      list.append(item);
    }
    card.append(list);
    card.append(el("p", "hint", "Double-cliquez sur une piste pour la lancer."));
  }

  const nowPlaying = el("div", "now-playing", "Aucune piste en lecture");
  nowPlaying.id = "player-now";
  card.append(nowPlaying);

  const controls = el("div", "row");
  const previous = el("button", undefined, "⏮");
  previous.addEventListener("click", () => void skip("player_previous"));
  const toggle = el("button", "primary", state.playerPlaying ? "Pause" : "Lecture");
  toggle.id = "player-toggle";
  toggle.addEventListener("click", () => void togglePlayback());
  const stop = el("button", undefined, "Stop");
  stop.addEventListener("click", () => void invoke("player_stop"));
  const next = el("button", undefined, "⏭");
  next.addEventListener("click", () => void skip("player_next"));
  const clock = el("span", "clock", formatClock(state.playerPositionMs));
  clock.id = "player-clock";
  controls.append(previous, toggle, stop, next, clock);
  card.append(controls);

  const options = el("div", "row");
  const shuffle = el("label", "check");
  const shuffleBox = el("input");
  shuffleBox.type = "checkbox";
  shuffleBox.checked = state.playerShuffle;
  shuffleBox.addEventListener("change", () => void setShuffle(shuffleBox.checked));
  shuffle.append(shuffleBox, " Aléatoire");
  const repeat = el("button", undefined, repeatLabel(state.playerRepeat));
  repeat.addEventListener("click", () => void cycleRepeat());
  const volumeLabel = el("label", "check", "Volume ");
  const volume = el("input");
  volume.type = "range";
  volume.min = "0";
  volume.max = "100";
  volume.value = String(Math.round(state.playerVolume * 100));
  volume.addEventListener("input", () => {
    state.playerVolume = Number(volume.value) / 100;
    void invoke("player_set_volume", { volume: state.playerVolume });
  });
  volumeLabel.append(volume);
  options.append(shuffle, repeat, volumeLabel);
  card.append(options);

  return card;
}

function statusText(status: Availability | undefined): string {
  switch (status) {
    case "online":
      return "Disponible";
    case "offline":
      return "Injoignable (disque débranché ou réseau indisponible)";
    default:
      return "Vérification…";
  }
}

function sourceRow(source: MusicSource): HTMLElement {
  const status = state.availability.get(source.id);
  const item = el("li", "source");
  item.append(el("span", `dot ${status ?? "checking"}`));

  const text = el("div");
  text.append(el("div", "name", source.label));
  text.append(el("div", "path", source.path));
  text.append(el("div", status === "offline" ? "status offline" : "status", statusText(status)));
  item.append(text);

  const actions = el("div", "row");
  const relink = el("button", undefined, "Relier…");
  relink.title = "Le disque a changé de lettre ou le dossier a été déplacé : réaligner sans réindexer";
  relink.disabled = state.busy;
  relink.addEventListener("click", () => void relinkFolder(source));
  const remove = el("button", "danger", "Retirer");
  remove.title = "Retire le dossier de la bibliothèque sans toucher aux fichiers";
  remove.addEventListener("click", () => void removeFolder(source));
  actions.append(relink, remove);
  item.append(actions);
  return item;
}

function sourcesCard(): HTMLElement {
  const card = el("section", "card");
  const head = el("div", "row spread");
  const titles = el("div");
  titles.append(el("h2", undefined, "Dossiers sources"));
  titles.append(
    el(
      "p",
      "hint",
      "Ajoutez autant de dossiers que nécessaire : disque local, disque externe, partage réseau (\\\\NAS\\Musique).",
    ),
  );
  const add = el("button", "primary", "Ajouter un dossier…");
  add.disabled = state.busy;
  add.addEventListener("click", () => void addFolder());
  head.append(titles, add);
  card.append(head);

  if (state.sources.length === 0) {
    card.append(el("div", "empty", "Aucun dossier pour l'instant."));
  } else {
    const list = el("ul", "sources");
    for (const source of state.sources) list.append(sourceRow(source));
    card.append(list);
  }

  const message = el("p", state.message ? `message ${state.message.kind}` : "message");
  message.setAttribute("aria-live", "polite");
  message.textContent = state.message?.text ?? "";
  card.append(message);
  return card;
}

function libraryCard(): HTMLElement {
  const card = el("section", "card");
  const head = el("div", "row spread");
  const titles = el("div");
  titles.append(el("h2", undefined, "Bibliothèque"));
  titles.append(
    el(
      "p",
      "hint",
      "Détecte les artistes, albums et disques dans les dossiers sources ci-dessus (jamais dans les tags).",
    ),
  );
  const scanButton = el(
    "button",
    "primary",
    state.scanning ? "Scan en cours…" : "Scanner la bibliothèque",
  );
  scanButton.disabled = state.busy || state.scanning || state.sources.length === 0;
  scanButton.addEventListener("click", () => void scanLibrary());
  head.append(titles, scanButton);
  card.append(head);

  const report = state.scanReport;
  if (!report) {
    card.append(el("div", "empty", "Pas encore scannée."));
    return card;
  }

  card.append(
    el(
      "p",
      "hint",
      `${report.totalFiles} piste(s) · ${report.artistCount} artiste(s) · ${formatDuration(report.elapsedMs)}`,
    ),
  );

  const problems = report.sources.filter((s) => s.unavailable || s.stats.skipped > 0);
  if (problems.length > 0) {
    const list = el("ul", "scan-issues");
    for (const source of problems) {
      const line = source.unavailable
        ? `« ${source.label} » est injoignable (disque débranché ou réseau indisponible).`
        : `« ${source.label} » : ${source.stats.skipped} fichier(s) ignoré(s) (vides ou en double).`;
      list.append(el("li", undefined, line));
    }
    card.append(list);
  }

  if (report.artists.length > 0) {
    const list = el("ul", "artist-list");
    for (const artist of report.artists) {
      const item = el("li", "artist-row");
      item.append(el("span", "name", artist.name));
      const albumWord = artist.albumCount > 1 ? "albums" : "album";
      const trackWord = artist.trackCount > 1 ? "pistes" : "piste";
      item.append(
        el("span", "count", `${artist.albumCount} ${albumWord} · ${artist.trackCount} ${trackWord}`),
      );
      list.append(item);
    }
    card.append(list);
  } else {
    card.append(el("div", "empty", "Aucun artiste détecté."));
  }

  return card;
}

function systemCard(): HTMLElement {
  const card = el("section", "card");
  card.append(el("h2", undefined, "Système"));
  const list = el("dl", "info");
  const add = (label: string, value: string | HTMLElement): void => {
    list.append(el("dt", undefined, label));
    const dd = el("dd");
    dd.append(value);
    list.append(dd);
  };
  add("Version", state.info ? `${state.info.version} (${state.info.os} ${state.info.arch})` : "…");
  add("Données", state.info?.dataDir ?? "…");
  add("Affichage", fpsNode);
  card.append(list);
  return card;
}

function render(): void {
  const page = el("div", "page");
  const brand = el("header", "brand");
  const title = el("h1");
  title.append("n7", el("span", undefined, "Folder"), " Player");
  brand.append(title, el("p", undefined, "Votre musique, telle qu'elle est rangée dans vos dossiers."));
  page.append(brand, sourcesCard(), libraryCard(), playerCard(), systemCard());
  root?.replaceChildren(page);
}

function startFrameMeter(): void {
  let frames = 0;
  let windowStart = performance.now();
  const tick = (now: number): void => {
    frames += 1;
    if (now - windowStart >= 500) {
      fpsNode.textContent = `${Math.round((frames * 1000) / (now - windowStart))} fps`;
      frames = 0;
      windowStart = now;
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

async function listenToPlayer(): Promise<void> {
  await listen<PlayerTick>("player-tick", (event) => {
    const tick = event.payload;
    const pathChanged = tick.path !== state.playerPath;
    state.playerPath = tick.path;
    state.playerPositionMs = tick.positionMs;
    state.playerPlaying = tick.playing;
    if (pathChanged) {
      render();
    } else {
      paintPlayerClock();
    }
    if (tick.ended) {
      // Fin naturelle de piste : on enchaîne (la file gère aléatoire, répétition et fin de liste).
      void invoke("player_next").catch((error: unknown) => say("error", errorText(error)));
    }
  });
  await listen<{ message: string }>("player-error", (event) => {
    say("error", event.payload.message);
  });
}

async function boot(): Promise<void> {
  render();
  startFrameMeter();
  try {
    await listenToPlayer();
    state.info = await invoke<AppInfo>("app_info");
    applySources(await invoke<MusicSource[]>("get_sources"));
  } catch (error) {
    say("error", errorText(error));
  }
}

void boot();

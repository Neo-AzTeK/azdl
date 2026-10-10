/** Normalize the different Steam and installed-game records for GameCard. */
export function normalizeGameCard(game = {}) {
  const genres = Array.isArray(game.genres)
    ? [...new Set(game.genres
        .map((genre) => typeof genre === 'string' ? genre : genre?.description)
        .filter((genre) => typeof genre === 'string' && genre.trim())
        .map((genre) => genre.trim()))]
    : [];

  return {
    ...game,
    title: game.title || game.name || game.id || 'Unknown game',
    source_id: cleanLabel(game.source_id || game.source),
    version: cleanLabel(game.version),
    file_size: cleanLabel(game.file_size),
    genres,
    addons_count: Number.isFinite(Number(game.addons_count)) ? Number(game.addons_count) : 0,
    runner: cleanLabel(game.runner),
  };
}

/** The sole game-card tag is storage size. */
export function getGameCardMetadataTags(game, steamStorageSize = null) {
  const size = game.file_size || cleanLabel(steamStorageSize);
  return size ? [{ type: 'size', label: size }] : [];
}

function cleanLabel(value) {
  if (typeof value !== 'string') return null;
  const label = value.trim();
  return label ? label : null;
}

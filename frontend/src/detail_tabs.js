/** Return a game identity that survives catalog-to-library ID normalization. */
export function detailGameIdentity(game) {
  const steamAppId = Number(game?.steam_app_id);
  if (Number.isSafeInteger(steamAppId) && steamAppId > 0) {
    return `steam:${steamAppId}`;
  }

  const qualifiedId = String(game?.qualified_id || '').trim().toLowerCase();
  if (qualifiedId) return qualifiedId;

  const sourceId = String(game?.source_id || '').trim().toLowerCase();
  const id = String(game?.id || '').trim().toLowerCase();
  return sourceId || id ? `${sourceId}:${id}` : '';
}

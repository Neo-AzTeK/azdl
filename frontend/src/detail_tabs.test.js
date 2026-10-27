import { describe, expect, it } from 'vitest';
import { detailGameIdentity } from './detail_tabs.js';

describe('detailGameIdentity', () => {
  it('keeps the same Steam game identity when its catalog ID becomes a library ID', () => {
    const catalogGame = { id: 'steam:1234', steam_app_id: 1234 };
    const installedGame = { id: 'local-game-id', steam_app_id: 1234 };

    expect(detailGameIdentity(installedGame)).toBe(detailGameIdentity(catalogGame));
  });

  it('distinguishes two installed Steam games', () => {
    expect(detailGameIdentity({ id: 'game-a', steam_app_id: 1234 }))
      .not.toBe(detailGameIdentity({ id: 'game-b', steam_app_id: 5678 }));
  });

  it('uses the qualified source identity before a mutable local ID', () => {
    expect(detailGameIdentity({ id: 'local-a', qualified_id: 'Steam:1234' }))
      .toBe(detailGameIdentity({ id: 'local-b', qualified_id: 'steam:1234' }));
  });
});

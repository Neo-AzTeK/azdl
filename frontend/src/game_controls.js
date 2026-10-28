/** Match a management command against its observed backend read-back. */
export function controlActionConfirmed(action, before, after) {
  const observations = {
    'overlay-toggle': [before?.overlay?.enabled, after?.overlay?.enabled, 'toggle'],
    'patch-toggle': [before?.patch === 'enabled', after?.patch === 'enabled', 'toggle'],
    'steamstub-unpack': [null, after?.steamstub?.applied, true],
    'steamstub-rollback': [null, after?.steamstub?.applied, false],
    'dlc-license-patch': [null, after?.dlc_patch_applied, true],
    'dlc-license-rollback': [null, after?.dlc_patch_applied, false],
    'goldberg-sync': [null, after?.goldberg_has_settings, true],
    'goldberg-rollback': [null, after?.goldberg_has_settings, false],
    'ea-heal': [null, after?.ea_heal_applied, true],
    'ea-rollback': [null, after?.ea_heal_applied, false],
    'install-vcredist': [null, after?.runtime?.status?.vcredist_x64, true],
    'install-directx': [null, after?.runtime?.status?.directx, true],
  };
  if (!observations[action]) return true;
  const [previous, actual, direction] = observations[action];
  if (typeof actual !== 'boolean') return false;
  return actual === (direction === 'toggle' ? !previous : direction);
}

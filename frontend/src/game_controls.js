/** Match a management command against its observed backend read-back. */
export function controlActionConfirmed(action, after) {
  const observations = {
    'install-vcredist': after?.runtime?.status?.vcredist_x64,
    'install-directx': after?.runtime?.status?.directx,
    'steamstub-unpack': after?.steamstub?.applied === true,
    'steamstub-rollback': after?.steamstub?.applied === false,
    'dlc-license-patch': after?.dlc_patch_applied === true,
    'dlc-license-rollback': after?.dlc_patch_applied === false,
    'goldberg-sync': after?.goldberg_has_settings === true,
    'goldberg-rollback': after?.goldberg_has_settings === false,
    'ea-heal': after?.ea_heal_applied === true,
    'ea-rollback': after?.ea_heal_applied === false,
  };
  if (!(action in observations)) return true;
  return observations[action] === true;
}

/** Match a management command against its observed backend read-back. */
export function controlActionConfirmed(action, after) {
  const observations = {
    'install-vcredist': after?.runtime?.status?.vcredist_x64,
    'install-directx': after?.runtime?.status?.directx,
  };
  if (!(action in observations)) return true;
  return observations[action] === true;
}

/** Reuse unchanged JSON-shaped state branches to avoid needless Svelte updates. */
export function reconcileData(previous, next) {
  if (Object.is(previous, next)) return previous;
  if (
    previous === null ||
    next === null ||
    typeof previous !== 'object' ||
    typeof next !== 'object'
  ) {
    return next;
  }

  const previousIsArray = Array.isArray(previous);
  if (previousIsArray !== Array.isArray(next)) return next;

  if (previousIsArray) {
    let result = previous.length === next.length ? null : previous.slice(0, next.length);
    for (let index = 0; index < next.length; index += 1) {
      const value = index < previous.length
        ? reconcileData(previous[index], next[index])
        : next[index];
      if (index >= previous.length || value !== previous[index]) {
        if (!result) result = previous.slice();
        result[index] = value;
      }
    }
    return result || previous;
  }

  const previousKeys = Object.keys(previous);
  const nextKeys = Object.keys(next);
  const sameKeys = previousKeys.length === nextKeys.length &&
    nextKeys.every((key) => Object.prototype.hasOwnProperty.call(previous, key));
  let result = sameKeys ? null : {};
  for (const key of nextKeys) {
    const hasPrevious = Object.prototype.hasOwnProperty.call(previous, key);
    const value = hasPrevious ? reconcileData(previous[key], next[key]) : next[key];
    if (!sameKeys || !hasPrevious || value !== previous[key]) {
      if (!result) result = { ...previous };
      Object.defineProperty(result, key, {
        value,
        enumerable: true,
        configurable: true,
        writable: true,
      });
    }
  }
  return result || previous;
}

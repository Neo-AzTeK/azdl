/** Present canonical Windows paths without their extended-length namespace prefix. */
export function displayPath(path) {
  if (typeof path !== 'string') return '';
  if (/^\\\\\?\\UNC\\/i.test(path)) return `\\\\${path.slice(8)}`;
  if (/^\\\\\?\\[a-z]:\\/i.test(path)) return path.slice(4);
  return path;
}

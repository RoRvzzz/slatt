const { execFile } = require('node:child_process');

function parseDiagnostic(stderr, source) {
  const match = /^slatt: -:([1-9]\d*):([1-9]\d*): ([^\r\n]+)$/m.exec(stderr);
  if (!match) return undefined;
  const lines = source.split(/\r?\n/);
  const line = Math.min(Number(match[1]) - 1, lines.length - 1);
  const characters = [...lines[line]];
  const column = Math.min(Number(match[2]) - 1, characters.length);
  const start = characters.slice(0, column).join('').length;
  const end = start + (characters[column]?.length ?? 0);
  return { line, start, end, message: match[3] };
}

function checkRules(executable, source, signal, sourcePath) {
  const toolError = (message) => ({ line: 0, start: 0, end: 0, message, toolError: true });
  if (typeof executable !== 'string' || executable.includes('\0')) {
    return Promise.resolve(toolError('Set slatt.executablePath to a valid executable path.'));
  }
  return new Promise((resolve) => {
    const args = ['check', '-'];
    if (sourcePath) args.push('--stdin-path', sourcePath);
    const child = execFile(executable, args, {
      windowsHide: true,
      encoding: 'utf8',
      timeout: 5000,
      maxBuffer: 64 * 1024,
      signal,
    }, (error, _stdout, stderr) => {
      if (!error || signal?.aborted) return resolve(undefined);
      const diagnostic = parseDiagnostic(stderr, source);
      if (diagnostic) return resolve(diagnostic);
      const projectError = /^slatt: ([^\r\n]+)/m.exec(stderr);
      if (projectError) return resolve({ line: 0, start: 0, end: 0, message: projectError[1] });
      const message = error.code === 'ENOENT'
        ? 'Slatt executable could not be found. Set slatt.executablePath to enable diagnostics.'
        : 'Slatt failed to check this file. Check slatt.executablePath and run slatt check in a terminal.';
      resolve(toolError(message));
    });
    // The compiler can exit before it consumes stdin.
    child.stdin?.on('error', () => {});
    child.stdin?.end(source, 'utf8');
  });
}

module.exports = { checkRules, parseDiagnostic };

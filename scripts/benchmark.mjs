// Generate a repeatable 100,000-line text case outside the tracked source tree.
import { mkdir, writeFile } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const dir = resolve(root, '.tools', 'benchmark');
await mkdir(dir, { recursive: true });
const lines = Array.from({ length: 100_000 }, (_, i) =>
  `${String(i).padStart(6, '0')} | Rust Compare 中文基准 | ${'original content '.repeat(4)}\n`);
const left = lines.join('');
const right = lines.map((line, i) => i % 4096 === 0 ? line.replace('original', 'modified') : line).join('');
const leftPath = resolve(dir, 'left.txt');
const rightPath = resolve(dir, 'right.txt');
await writeFile(leftPath, left);
await writeFile(rightPath, right);
const profile = process.argv[2] === 'release' ? 'release' : 'debug';
const exe = resolve(root, 'target', profile, process.platform === 'win32' ? 'bcompare.exe' : 'bcompare');
const started = performance.now();
const child = spawn(exe, ['diff', leftPath, rightPath, '--json'], { cwd: root, windowsHide: true });
let output = '';
child.stdout.on('data', chunk => { output += chunk; });
child.stderr.pipe(process.stderr);
child.on('error', error => { console.error(error.message); process.exitCode = 2; });
child.on('exit', code => {
  if (code !== 1) { console.error(`Expected differences (exit 1), got ${code}`); process.exitCode = 2; return; }
  try {
    const result = JSON.parse(output);
    console.log(JSON.stringify({ profile, lines: lines.length, bytes_per_file: Buffer.byteLength(left),
      differences: result.hunks.length, coarse_alignment: result.coarse_alignment,
      elapsed_ms: Math.round(performance.now() - started) }, null, 2));
    if (result.hunks.length !== 25) throw new Error('Unexpected difference count');
  } catch (error) { console.error(error.message); process.exitCode = 2; }
});

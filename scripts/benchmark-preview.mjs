// Reproducible bounded-preview and directory benchmarks; generated files stay in .tools/.
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import assert from 'node:assert/strict';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
await mkdir(resolve(root, '.tools'), { recursive: true });
const directory = await mkdtemp(resolve(root, '.tools/preview-benchmark-'));
const profile = process.argv[2] === 'release' ? 'release' : 'debug';
const exe = resolve(root, 'target', profile, process.platform === 'win32' ? 'bcompare.exe' : 'bcompare');
const invoke = promisify(execFile);
async function run(args, expectedExit) {
  const start = performance.now();
  let output;
  try {
    output = (await invoke(exe, args, { cwd: root, windowsHide: true, maxBuffer: 16 * 1024 * 1024 })).stdout;
    assert.equal(expectedExit, 0, 'Expected a nonzero exit status');
  } catch (error) {
    if (error.code !== expectedExit) throw error;
    output = error.stdout;
  }
  return { result: JSON.parse(output), elapsed_ms: Math.round(performance.now() - start) };
}

const line = `sample ${'x'.repeat(120)}\n`;
const text = line.repeat(262_144);
const leftFile = resolve(directory, 'large-left.txt');
const rightFile = resolve(directory, 'large-right.txt');
await Promise.all([writeFile(leftFile, text), writeFile(rightFile, text.slice(0, -5) + 'diff\n')]);
const preview = await run(['diff', leftFile, rightFile, '--json'], 1);
assert.equal(preview.result.mode, 'read_only_preview');
assert.equal(preview.result.equal, false);
assert.equal(preview.result.hunks.length, 0);
assert.equal(preview.result.left_preview.has_more, true);
assert.ok(preview.result.left_preview.end_byte <= 1024 * 1024);

const leftDir = resolve(directory, 'left');
const rightDir = resolve(directory, 'right');
await Promise.all([mkdir(leftDir), mkdir(rightDir)]);
for (let start = 0; start < 1000; start += 25) {
  await Promise.all(Array.from({ length: Math.min(25, 1000 - start) }, async (_, offset) => {
    const index = start + offset;
    const name = `file-${String(index).padStart(4, '0')}.txt`;
    await Promise.all([writeFile(resolve(leftDir, name), 'original\n'),
      writeFile(resolve(rightDir, name), index % 100 === 0 ? 'modified\n' : 'original\n')]);
  }));
}
const scan = await run(['dir', leftDir, rightDir, '--json'], 1);
assert.equal(scan.result.complete, true);
assert.equal(scan.result.entries.length, 1000);
assert.equal(scan.result.entries.filter(entry => entry.status === 'Different').length, 10);
console.log(JSON.stringify({ profile,
  preview: { bytes_per_file: Buffer.byteLength(text), lines_per_file: 262_144,
    first_page_bytes: preview.result.left_preview.end_byte, full_file_equal: false,
    first_page_hunks: 0, elapsed_ms: preview.elapsed_ms },
  directory: { entries: 1000, differences: 10, elapsed_ms: scan.elapsed_ms },
}, null, 2));

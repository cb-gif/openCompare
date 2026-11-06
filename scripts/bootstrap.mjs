// Optional project-local Rust installation. Does not change the user's PATH.
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawn } from 'node:child_process';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const tools = resolve(root, '.tools');
await mkdir(tools, { recursive: true });
const url = 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe';
console.log('Downloading Rust installer from static.rust-lang.org...');
const response = await fetch(url);
if (!response.ok) throw new Error(`Download failed: HTTP ${response.status}`);
const installer = resolve(tools, 'rustup-init.exe');
await writeFile(installer, Buffer.from(await response.arrayBuffer()));
const child = spawn(installer, [
  '-y', '--no-modify-path', '--default-host', 'x86_64-pc-windows-msvc',
  '--default-toolchain', 'stable', '--profile', 'minimal',
  '--component', 'rustfmt,clippy',
], {
  cwd: root,
  stdio: 'inherit',
  env: { ...process.env, CARGO_HOME: resolve(tools, 'cargo'), RUSTUP_HOME: resolve(tools, 'rustup') },
});
child.on('error', error => { console.error(error.message); process.exitCode = 1; });
child.on('exit', code => { process.exitCode = code ?? 1; });


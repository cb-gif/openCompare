// Repository documentation checks; no network access or third-party dependencies.
import assert from 'node:assert/strict';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, extname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const taskPattern = /\b(?:BASE|DOC|FND|UX|TXT|DIR|OPS|SES|GIT|VAL|REL|RET|RPT|EXT)-\d{2}\b/g;

function withoutCode(markdown) {
  return markdown.replace(/^(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\1\s*$/gm, '')
    .replace(/`[^`\n]*`/g, '');
}

function checkLinks(markdown, file, exists) {
  const errors = [];
  // shortcut: inline Markdown links only; add reference-link support when docs use it.
  const links = withoutCode(markdown).matchAll(/!?\[[^\]\n]*\]\((?:<([^>]+)>|([^\s)]+))(?:\s+"[^"]*")?\)/g);
  for (const match of links) {
    const target = match[1] ?? match[2];
    if (/^(?:[a-z][a-z\d+.-]*:|#|\/\/)/i.test(target)) continue;
    let path;
    try { path = decodeURIComponent(target.split(/[?#]/, 1)[0]); }
    catch { errors.push(`${file}: invalid link encoding: ${target}`); continue; }
    if (path && !exists(resolve(dirname(file), path))) errors.push(`${file}: missing link: ${target}`);
  }
  return errors;
}

function parseTasks(plan) {
  const definitions = [...plan.matchAll(/^- \[([ x])\] \*\*([A-Z]+-\d{2}) ([^\n]+)\*\*\s*$/gm)];
  return definitions.map((m, index) => {
    const body = plan.slice(m.index + m[0].length, definitions[index + 1]?.index ?? plan.length);
    const deps = body.match(/依赖：([^。\n]*)/)?.[1];
    const evidence = body.match(/^[ \t]*- 证据：(.+)$/m)?.[1];
    return { id: m[2], checked: m[1] === 'x', deps: deps?.match(taskPattern) ?? [], hasDeps: deps !== undefined, evidence };
  });
}

function checkTasks(plan, documents) {
  const errors = [];
  const tasks = parseTasks(plan);
  const known = new Map();
  if (!tasks.length) errors.push('PLAN: no task definitions');
  for (const task of tasks) {
    if (known.has(task.id)) errors.push(`PLAN: duplicate task ${task.id}`);
    known.set(task.id, task);
    if (!task.hasDeps) errors.push(`PLAN: ${task.id} has no dependency field`);
    if (!task.evidence) errors.push(`PLAN: ${task.id} has no evidence field`);
    if (task.checked && /待补|待验证|待验收|尚未|TODO/i.test(task.evidence ?? '')) errors.push(`PLAN: ${task.id} checked without completed evidence`);
  }
  for (const task of tasks) for (const dep of task.deps) {
    if (!known.has(dep)) errors.push(`PLAN: ${task.id} depends on unknown ${dep}`);
    else if (task.checked && !known.get(dep).checked) errors.push(`PLAN: ${task.id} checked before dependency ${dep}`);
  }
  const visiting = new Set();
  const visited = new Set();
  function visit(id) {
    if (visiting.has(id)) { errors.push(`PLAN: dependency cycle at ${id}`); return; }
    if (visited.has(id) || !known.has(id)) return;
    visiting.add(id);
    for (const dep of known.get(id).deps) visit(dep);
    visiting.delete(id);
    visited.add(id);
  }
  for (const task of tasks) visit(task.id);
  for (const [file, content] of documents) {
    for (const id of new Set(withoutCode(content).match(taskPattern) ?? [])) {
      if (!known.has(id)) errors.push(`${file}: unknown task reference ${id}`);
    }
  }
  return { errors, tasks };
}

function selfTest() {
  const file = resolve(root, 'virtual/doc.md');
  const target = resolve(root, 'virtual/ok.md');
  assert.deepEqual(checkLinks('[ok](ok.md#part) [web](https://example.com) ` [skip](missing.md) `', file, p => p === target), []);
  assert.equal(checkLinks('[bad](missing.md)', file, () => false).length, 1);
  assert.equal(checkLinks('[space](<with space.md>)', file, p => p.endsWith('with space.md')).length, 0);
  assert.equal(checkLinks('[bad](%XX.md)', file, () => true).length, 1);
  assert.equal(checkLinks('```text\n[example](missing.md)\n```\n', file, () => false).length, 0);
  const task = (id, deps = '—', checked = false, evidence = 'test log') => `- [${checked ? 'x' : ' '}] **${id} Example**\n  - 依赖：${deps}。\n  - 证据：${evidence}\n`;
  const valid = task('DOC-01', '—', true) + task('DOC-02', 'DOC-01');
  assert.deepEqual(checkTasks(valid, [['doc', 'DOC-01, UTF-16']]).errors, []);
  assert.ok(checkTasks(valid + task('DOC-01'), []).errors.some(e => e.includes('duplicate')));
  assert.ok(checkTasks(task('DOC-01', 'DOC-99'), []).errors.some(e => e.includes('unknown')));
  assert.ok(checkTasks(task('DOC-01', 'DOC-02') + task('DOC-02', 'DOC-01'), []).errors.some(e => e.includes('cycle')));
  assert.ok(checkTasks(task('DOC-01', '—', true, '待补'), []).errors.some(e => e.includes('evidence')));
  assert.ok(checkTasks(task('DOC-01') + task('DOC-02', 'DOC-01', true), []).errors.some(e => e.includes('before dependency')));
  assert.ok(checkTasks(valid, [['doc', 'FND-99']]).errors.some(e => e.includes('unknown task reference')));
  assert.ok(checkTasks('', []).errors.some(e => e.includes('no task')));
  console.log('Documentation checker self-test passed (13 cases).');
}

function markdownFiles(path) {
  return readdirSync(path, { withFileTypes: true }).flatMap(entry => {
    const file = join(path, entry.name);
    if (entry.isDirectory()) return markdownFiles(file);
    return entry.isFile() && extname(file) === '.md' ? [file] : [];
  });
}

if (process.argv.includes('--self-test')) {
  selfTest();
} else {
  const files = ['README.md', 'AGENTS.md'].map(p => resolve(root, p));
  for (const dir of ['docs/dev', '.agents/skills/opencompare-development']) files.push(...markdownFiles(resolve(root, dir)));
  const documents = files.map(file => [file, readFileSync(file, 'utf8')]);
  const { errors, tasks } = checkTasks(readFileSync(resolve(root, 'docs/dev/plan.md'), 'utf8'), documents);
  for (const [file, content] of documents) errors.push(...checkLinks(content, file, existsSync));
  if (errors.length) {
    for (const error of errors) console.error(error.replaceAll(root, '.'));
    process.exitCode = 1;
  } else {
    console.log(`Checked ${documents.length} Markdown files and ${tasks.length} task nodes; local links and task graph passed.`);
    for (const [name, select] of [
      ['Prototype baseline', t => t.id.startsWith('BASE-')],
      ['Documentation', t => t.id.startsWith('DOC-')],
      ['Target product', t => !/^(BASE|DOC)-/.test(t.id)],
    ]) {
      const group = tasks.filter(select);
      console.log(`${name}: ${group.filter(t => t.checked).length}/${group.length} checked.`);
    }
    console.log('Checks do not verify evidence truth, remote URLs, or anchors.');
  }
}

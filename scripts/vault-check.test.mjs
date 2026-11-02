// End-to-end tests for the vault gate's ownership check; vault-check.mjs is a
// top-level program, so each test runs it against a throwaway git repo.
import test from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const HERE = dirname(fileURLToPath(import.meta.url))
const GIT = ['-c', 'user.name=t', '-c', 'user.email=t@t']

const run = (cwd, cmd, args) => spawnSync(cmd, args, { cwd, encoding: 'utf8' })
const check = (dir, ...args) => run(dir, process.execPath, ['scripts/vault-check.mjs', ...args])

// Fixture: vault `x` covers a.rs; b.rs is tracked. `extra` is appended to the
// frontmatter. Lock is stamped with --write, then everything is committed.
function withRepo(extra, fn) {
  const dir = mkdtempSync(join(tmpdir(), 'vault-check-'))
  try {
    mkdirSync(join(dir, 'scripts'))
    mkdirSync(join(dir, 'docs', 'vault'), { recursive: true })
    for (const f of ['vault-check.mjs', 'vault-cites.mjs']) copyFileSync(join(HERE, f), join(dir, 'scripts', f))
    writeFileSync(join(dir, 'a.rs'), 'fn a() {}\n')
    writeFileSync(join(dir, 'b.rs'), 'fn b() {}\n')
    // scripts/ stays untracked so it is not itself unclaimed code
    const page = (more) => `---\nvault: x\ncovers:\n  - a.rs\n${more}---\n\n# x\n`
    const vaultPath = join(dir, 'docs', 'vault', 'x.md')
    writeFileSync(vaultPath, page('')) // --write refuses bad lists, so stamp the plain page first
    assert.equal(run(dir, 'git', ['init', '-q']).status, 0)
    run(dir, 'git', ['add', 'a.rs', 'b.rs', 'docs'])
    const w = check(dir, '--write')
    assert.equal(w.status, 0, w.stderr)
    writeFileSync(vaultPath, page(extra))
    run(dir, 'git', ['add', 'a.rs', 'b.rs', 'docs'])
    assert.equal(run(dir, 'git', [...GIT, 'commit', '-qm', 'fixture']).status, 0)
    return fn(dir)
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test('fails on an unclaimed code file // b.rs in no list must break the gate', () => {
  withRepo('', (dir) => {
    const r = check(dir)
    assert.equal(r.status, 1)
    assert.match(r.stderr, /b\.rs/)
    assert.ok(r.stderr.includes('/\\.(rs|ts|tsx|go|mjs)$/'), r.stderr)
  })
})

test('passes when the file is unowned // allowlisting b.rs is the sanctioned way out', () => {
  withRepo('unowned:\n  - b.rs # no explanation needed\n', (dir) => {
    const r = check(dir)
    assert.equal(r.status, 0, r.stderr)
    assert.ok(r.stdout.includes('ownership: 1 covered, 1 unowned, 0 unclaimed'), r.stdout)
  })
})

test('rejects a path both covered and unowned // one file may not claim two statuses', () => {
  withRepo('unowned:\n  - a.rs\n  - b.rs\n', (dir) => {
    const r = check(dir)
    assert.equal(r.status, 1)
    assert.match(r.stderr, /pick one/)
  })
})

test('rejects an untracked unowned entry // a typo in unowned: must not silently allowlist nothing', () => {
  withRepo('unowned:\n  - b.rs\n  - ghost.rs\n', (dir) => {
    const r = check(dir)
    assert.equal(r.status, 1)
    assert.match(r.stderr, /ghost\.rs/)
  })
})

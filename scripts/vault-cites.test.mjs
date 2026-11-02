// Unit tests for the vault cite checker; each pins one rule of checkCites.
import test from 'node:test'
import assert from 'node:assert/strict'
import { checkCites, CITE_WINDOW } from './vault-cites.mjs'

const MD = 'docs/vault/page.md'
const LINK = '../../app/src-tauri/src/lib.rs'
const SRC = 'app/src-tauri/src/lib.rs'
const cite = (sym, n, text = 'lib.rs') => `[${sym}@${text}:${n}](${LINK})`
const run = (md, src, path = SRC) =>
  checkCites(md, MD, (p) => (p === path ? src : null))
const lines = (n, at = {}) =>
  Array.from({ length: n }, (_, i) => at[i + 1] ?? `// filler ${i + 1}`).join('\n')

test('window is 3 // pins the documented tolerance', () => {
  assert.equal(CITE_WINDOW, 3)
})

test('accepts symbol on, 3 above and 3 below the cited line // window edges are inclusive', () => {
  for (const at of [20, 17, 23]) {
    const r = run(`see ${cite('foo', 20)}`, lines(40, { [at]: 'fn foo() {}' }))
    assert.deepEqual(r, { cites: 1, errors: [] }, `at ${at}`)
  }
})

test('moved 10+ lines reports real line and hint // the gate must tell the author the fix', () => {
  const r = run(`x\n${cite('foo', 20)}`, lines(60, { 35: 'fn foo() {}' }))
  assert.equal(r.errors.length, 1)
  assert.match(r.errors[0], /^docs\/vault\/page\.md:2: /)
  assert.match(r.errors[0], /found at app\/src-tauri\/src\/lib\.rs:35 → foo@lib\.rs:35/)
})

test('just outside the window is moved // 4 lines off must fail', () => {
  const r = run(cite('foo', 20), lines(40, { 24: 'fn foo() {}' }))
  assert.match(r.errors[0], /found at .*:24/)
})

test('absent symbol is reported not found // renamed/deleted code must not pass', () => {
  const r = run(cite('foo', 5), lines(10))
  assert.equal(r.errors.length, 1)
  assert.match(r.errors[0], /'foo' not found in app\/src-tauri\/src\/lib\.rs/)
})

test('definition line preferred over earlier mention // hint must point at the definition', () => {
  const r = run(cite('foo', 5), lines(60, { 20: 'let x = foo();', 40: 'pub fn foo() {}' }))
  assert.match(r.errors[0], /→ foo@lib\.rs:40/)
})

test('word boundary // foo must not match foobar', () => {
  const r = run(cite('foo', 5), lines(10, { 5: 'fn foobar() {}' }))
  assert.match(r.errors[0], /not found/)
})

test('line 0 and past EOF fall into moved/gone // no separate bounds message', () => {
  const src = lines(10, { 8: 'fn foo() {}' })
  assert.match(run(cite('foo', 0), src).errors[0], /found at .*:8/)
  assert.match(run(cite('foo', 999), src).errors[0], /found at .*:8/)
  assert.match(run(cite('zzz', 999), src).errors[0], /not found/)
})

test('bare link text is rejected // old [lib.rs:146](…) form must fail', () => {
  const r = run(`[lib.rs:146](${LINK})`, lines(200))
  assert.equal(r.cites, 0)
  assert.equal(r.errors.length, 1)
  assert.match(r.errors[0], /bare file:line cite 'lib\.rs:146' — write it as \[symbol@lib\.rs:146\]\(link\)/)
})

test('bare token and range in prose are rejected // plain file:line rots too', () => {
  const r = run('per Cargo.toml:44-51 and lib.rs:146', lines(5))
  assert.equal(r.errors.length, 2)
  assert.match(r.errors[0], /'Cargo\.toml:44-51'/)
})

test('valid cite is never bare // its own file:line is masked', () => {
  const r = run(cite('foo', 3), lines(5, { 3: 'fn foo() {}' }))
  assert.deepEqual(r.errors, [])
})

test('untracked link target is rejected // readSource null = git does not track it', () => {
  const r = run(cite('foo', 3), lines(5, { 3: 'fn foo() {}' }), 'other.rs')
  assert.match(r.errors[0], /resolves to app\/src-tauri\/src\/lib\.rs, which git does not track/)
})

test('text path must be suffix of link target // cite text cannot name another file', () => {
  const r = run(cite('foo', 3, 'rpc_server.rs'), lines(5, { 3: 'fn foo() {}' }))
  assert.match(r.errors[0], /cite text 'rpc_server\.rs' does not match link target app\/src-tauri\/src\/lib\.rs/)
  assert.deepEqual(run(cite('foo', 3, 'src/lib.rs'), lines(5, { 3: 'fn foo() {}' })).errors, [])
  assert.equal(run(cite('foo', 3, 'ib.rs'), lines(5, { 3: 'fn foo() {}' })).errors.length, 1)
})

test('CRLF input and counts // Windows checkouts must parse the same', () => {
  const md = `a\r\n${cite('foo', 3)}\r\n${cite('bar', 4)}\r\n`
  const r = run(md, lines(5, { 3: 'fn foo() {}', 4: 'fn bar() {}' }).replace(/\n/g, '\r\n'))
  assert.deepEqual(r, { cites: 2, errors: [] })
})

test('page without cites // trivial page is clean', () => {
  assert.deepEqual(run('just prose\n', ''), { cites: 0, errors: [] })
})

#!/usr/bin/env node
// The vault freshness gate.
//
// docs/vault/*.md are hand-written explanations of the code, meant to be read
// INSTEAD of the source — that is the whole point, it is what makes an agent
// cheap. A vault file that lies is worse than no vault file at all: whoever
// trusts it plans against a fiction and never finds out. docs/MODULES.md is
// the cautionary tale — it claimed lib.rs was ~1760 lines while the real file
// had grown to 12,475, and nothing anywhere said so.
//
// So each vault file declares, in its frontmatter, which source files it
// covers (or, for code that needs no explanation, lists it under `unowned:`).
// This script does four independent things with that:
//
//   1. HASH CHECK — sha256 every covered file, compare against
//      docs/vault/.vault-lock.json. Catches drift no matter how it arrived:
//      an old commit, a merge, a separate worktree. This is the check that
//      answers "is the vault up to date RIGHT NOW".
//
//   2. DIFF CHECK (--diff-base <sha>) — if a covered file moved in this
//      change, the vault file that owns it must have moved too. Without this,
//      `--write` alone would re-stamp the lock and the gate would be theatre.
//
//   3. CITE CHECK — every `[symbol@file:line]` cite in a vault page still
//      points at what it names (vault-cites.mjs).
//
//   4. OWNERSHIP CHECK — every tracked code file is claimed by some vault's
//      `covers:` or allowlisted in some `unowned:`. Without it, new code
//      nobody claimed would sit outside the gate forever, unseen.
//
// Modes:
//   node scripts/vault-check.mjs                     verify (exit 1 on drift)
//   node scripts/vault-check.mjs --write             re-stamp the lock
//   node scripts/vault-check.mjs --diff-base <sha>   also run the diff check
//
// No dependencies on purpose: it runs on a fresh checkout with nothing
// installed, which is exactly when you want to know the vault is stale.
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import { join, resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { checkCites } from './vault-cites.mjs'

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const LOCK_PATH = join(REPO, 'docs', 'vault', '.vault-lock.json')
const LOCK_REL = 'docs/vault/.vault-lock.json'
// Extensions that count as "code an agent would otherwise have to read": the
// ownership check fails on any tracked match claimed by no vault, and the
// coverage notice reports it as a line count.
const CODE_EXT = /\.(rs|ts|tsx|go|mjs)$/

const IN_CI = !!process.env.GITHUB_ACTIONS

function git(...args) {
  return execFileSync('git', args, { cwd: REPO, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
}
function gitZ(...args) {
  return git(...args).split('\0').filter(Boolean)
}
function err(msg) {
  console.error(IN_CI ? `::error::${msg}` : `ERROR: ${msg}`)
}
function notice(msg) {
  console.log(IN_CI ? `::notice::${msg}` : msg)
}

// --- frontmatter -----------------------------------------------------------
// Deliberately not a YAML parser. The frontmatter is a name and two lists; a
// dependency for that would be the tail wagging the dog. Anything the vault
// files are not allowed to contain is a hard error rather than a silent
// mis-parse — a `covers:` entry that quietly vanishes is the one failure mode
// this whole script exists to prevent.
function parseFrontmatter(text, relPath) {
  if (!/^---\r?\n/.test(text)) {
    throw new Error(`${relPath}: missing '---' frontmatter block on line 1`)
  }
  const end = text.indexOf('\n---', 3)
  if (end === -1) throw new Error(`${relPath}: frontmatter is never closed with '---'`)
  const body = text.slice(text.indexOf('\n') + 1, end)
  const out = { vault: null, covers: [], unowned: [] }
  let listKey = null // list key the next '  - item' belongs to
  for (const raw of body.split('\n')) {
    const line = raw.replace(/\r$/, '')
    if (!line.trim() || line.trim().startsWith('#')) continue
    const item = line.match(/^\s+-\s+(.+?)\s*(?:\s#.*)?$/) // trailing '# reason' allowed
    if (item) {
      if (!listKey) throw new Error(`${relPath}: list item outside 'covers:'/'unowned:' — ${line.trim()}`)
      out[listKey].push(item[1].replace(/^["']|["']$/g, ''))
      continue
    }
    const kv = line.match(/^([a-z_]+):\s*(.*)$/)
    if (!kv) throw new Error(`${relPath}: cannot parse frontmatter line — ${line.trim()}`)
    const [, key, value] = kv
    listKey = key === 'covers' || key === 'unowned' ? key : null
    if (key === 'vault') out.vault = value.trim()
    else if (listKey) {
      if (value.trim()) throw new Error(`${relPath}: '${key}:' must be a block list, one '  - path' per line`)
    } else throw new Error(`${relPath}: unknown frontmatter key '${key}'`)
  }
  if (!out.vault) throw new Error(`${relPath}: frontmatter has no 'vault:' name`)
  if (!/^[a-z0-9-]+$/.test(out.vault)) throw new Error(`${relPath}: vault name '${out.vault}' must be kebab-case`)
  return out
}

function sha256(absPath) {
  return createHash('sha256').update(readFileSync(absPath)).digest('hex')
}

// --- load the vault --------------------------------------------------------
const vaultFiles = gitZ('ls-files', '-z', '--', 'docs/vault/*.md')
const vaults = []
const owner = new Map() // source path -> vault name (single owner, enforced below)
const unowned = new Map() // source path -> vault name that allowlists it (duplicates across vaults are harmless)
const problems = []

for (const rel of vaultFiles) {
  const fm = parseFrontmatter(readFileSync(join(REPO, rel), 'utf8'), rel)
  if (vaults.some((v) => v.name === fm.vault)) {
    problems.push(`duplicate vault name '${fm.vault}' (${rel})`)
    continue
  }
  // A vault with no `covers:` is legal and gates nothing — INDEX.md is the
  // routing table, it explains where to look, not what the code does.
  const files = fm.covers.length ? gitZ('ls-files', '-z', '--', ...fm.covers) : []
  const unmatched = fm.covers.filter((p) => !p.includes('*') && !files.includes(p))
  for (const p of unmatched) problems.push(`${rel}: covers '${p}', which git does not track`)
  for (const f of files) {
    const prev = owner.get(f)
    // One source file, one vault file. Shared ownership makes "which doc must
    // I update" ambiguous, and the diff check below would have no answer.
    if (prev) problems.push(`${f} is covered by both '${prev}' and '${fm.vault}' — pick one`)
    else owner.set(f, fm.vault)
  }
  const free = fm.unowned.length ? gitZ('ls-files', '-z', '--', ...fm.unowned) : []
  for (const p of fm.unowned.filter((p) => !p.includes('*') && !free.includes(p))) {
    problems.push(`${rel}: unowned '${p}', which git does not track`)
  }
  for (const f of free) if (!unowned.has(f)) unowned.set(f, fm.vault)
  vaults.push({ name: fm.vault, path: rel, files: files.sort() })
}
// Both lists are known only now. Covered AND unowned says two things at once.
for (const [f, name] of owner) {
  if (unowned.has(f)) problems.push(`${f} is both covered by '${name}' and unowned in '${unowned.get(f)}' — pick one`)
}

if (problems.length) {
  for (const p of problems) err(p)
  process.exit(1)
}

// --- hash check ------------------------------------------------------------
const fresh = {}
for (const v of vaults) {
  if (!v.files.length) continue
  fresh[v.name] = {}
  for (const f of v.files) fresh[v.name][f] = sha256(join(REPO, f))
}

const write = process.argv.includes('--write')
const baseIdx = process.argv.indexOf('--diff-base')
const diffBase = baseIdx !== -1 ? process.argv[baseIdx + 1] : null

if (write) {
  writeFileSync(LOCK_PATH, JSON.stringify(fresh, null, 2) + '\n')
  console.log(`re-stamped ${LOCK_REL} — ${Object.keys(fresh).length} vault files, ${owner.size} source files`)
  process.exit(0)
}

const lock = existsSync(LOCK_PATH) ? JSON.parse(readFileSync(LOCK_PATH, 'utf8')) : {}
let stale = false

for (const name of new Set([...Object.keys(fresh), ...Object.keys(lock)])) {
  const now = fresh[name]
  const was = lock[name]
  const vault = vaults.find((v) => v.name === name)
  if (!now) {
    err(`vault '${name}' is in ${LOCK_REL} but no longer covers anything — run --write`)
    stale = true
    continue
  }
  if (!was) {
    err(`vault '${name}' (${vault.path}) is not stamped in ${LOCK_REL} — run --write`)
    stale = true
    continue
  }
  const drifted = []
  for (const [f, sha] of Object.entries(now)) {
    if (!(f in was)) drifted.push(`  + ${f} (newly covered, never summarised)`)
    else if (was[f] !== sha) drifted.push(`  ~ ${f} (changed since the vault file was written)`)
  }
  for (const f of Object.keys(was)) if (!(f in now)) drifted.push(`  - ${f} (gone from the tree)`)
  if (drifted.length) {
    stale = true
    err(`${vault.path} is stale — the code it explains moved:`)
    for (const d of drifted) console.error(d)
  }
}

// --- cite check ------------------------------------------------------------
// The hash check proves a page was re-stamped, not that its line cites still
// point at what they name. Runs on every vault page, in every mode but --write.
const tracked = new Set(gitZ('ls-files', '-z'))
{
  const readSource = (p) => (tracked.has(p) ? readFileSync(join(REPO, p), 'utf8') : null)
  let checked = 0
  for (const v of vaults) {
    const r = checkCites(readFileSync(join(REPO, v.path), 'utf8'), v.path, readSource)
    checked += r.cites
    for (const e of r.errors) err(e)
    if (r.errors.length) stale = true
  }
  console.log(`cites: ${checked} checked`)
}

// --- ownership check -------------------------------------------------------
// Code that no vault covers and none allowlists is invisible to every check
// above. Runs in verify and --diff-base modes; --write exits before reaching it.
{
  const unclaimed = [...tracked].filter((f) => CODE_EXT.test(f) && !owner.has(f) && !unowned.has(f)).sort()
  for (const f of unclaimed) {
    err(`${f} is code (matches ${CODE_EXT}) but no vault covers it — add it to a vault's 'covers:' or, if it genuinely needs no explanation, to 'unowned:'`)
  }
  if (unclaimed.length) stale = true
  console.log(`ownership: ${owner.size} covered, ${unowned.size} unowned, ${unclaimed.length} unclaimed`)
}

// --- diff check ------------------------------------------------------------
// The hash check can be satisfied by running --write and changing nothing.
// This one cannot: it asks whether the prose moved in the same change as the
// code, which is the actual habit worth enforcing.
if (diffBase) {
  const changed = new Set(gitZ('diff', '--name-only', '-z', diffBase, 'HEAD'))
  const missing = new Map() // vault path -> [source files]
  for (const f of changed) {
    const name = owner.get(f)
    if (!name) continue
    const vault = vaults.find((v) => v.name === name)
    if (changed.has(vault.path)) continue
    if (!missing.has(vault.path)) missing.set(vault.path, [])
    missing.get(vault.path).push(f)
  }
  if (missing.size) {
    stale = true
    for (const [vaultPath, files] of missing) {
      err(`${vaultPath} was not updated, but this change touches the code it explains. Update it, run 'node scripts/vault-check.mjs --write', and commit both — or put [vault-skip] in the PR title if the change genuinely does not affect the explanation.`)
      for (const f of files) console.error(`  changed: ${f}`)
    }
  } else {
    console.log('diff check: every touched-and-covered file has an updated vault entry')
  }
}

// --- coverage notice -------------------------------------------------------
// Never a failure. It exists so that "the vault covers less and less of the
// tree" is a number someone can watch, instead of a slow surprise.
{
  const all = gitZ('ls-files', '-z').filter((f) => CODE_EXT.test(f))
  let total = 0
  let covered = 0
  for (const f of all) {
    const n = readFileSync(join(REPO, f), 'utf8').split('\n').length
    total += n
    if (owner.has(f)) covered += n
  }
  const pct = total ? Math.round((covered / total) * 100) : 0
  notice(
    `vault coverage: ${covered.toLocaleString()} / ${total.toLocaleString()} lines of code (${pct}%), ` +
      `${owner.size} files across ${vaults.length} vault files`
  )
}

if (stale) {
  err('vault is out of date — see above. Fix the prose, then: node scripts/vault-check.mjs --write')
  process.exit(1)
}
console.log('vault is up to date')
